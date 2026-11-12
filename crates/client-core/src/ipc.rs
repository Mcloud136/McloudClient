//! 内核侧 IPC 方法处理。
//!
//! 提供 `status` / `reload` / `metrics` / `node_probe` / `nodes` /
//! `probe_nodes` / `select_node`。`reload` 复用与定时重载相同的
//! 「先构建、后替换」路径，失败时保留现有路由器。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use client_sidecar::{
    EventSink, Handler, IpcError,
    contract::{
        ERR_FORBIDDEN_TARGET, ERR_INVALID_PARAMS, ERR_UNKNOWN_GROUP, ERR_UNKNOWN_METHOD,
        ERR_UNKNOWN_NODE, MAX_PROBE_TIMEOUT_MS, METHOD_METRICS, METHOD_NODE_PROBE, METHOD_NODES,
        METHOD_PROBE_NODES, METHOD_RELOAD, METHOD_SELECT_NODE, METHOD_STATUS, NodeProbeOutcome,
        NodeProbeParams, NodeProbeResult, ProbeNodesParams, ProbeNodesResult, SelectNodeParams,
        SelectNodeResult, is_loopback_target,
    },
};
use futures::future::join_all;
use serde_json::{Value, json};
use tokio::net::TcpStream;

use crate::{context::AppContext, geo::GeoData, reload, router::SelectionError};

pub struct KernelHandler {
    context: Arc<AppContext>,
    source: reload::Source,
    geo: Arc<GeoData>,
    version: AtomicU64,
    started: Instant,
    /// 最近一次 `probe_nodes` 的结果，按节点名索引；不写健康表，避免污染 `url-test`。
    probe_cache: Mutex<HashMap<String, NodeProbeOutcome>>,
}

impl KernelHandler {
    #[must_use]
    pub fn new(context: Arc<AppContext>, source: reload::Source, geo: Arc<GeoData>) -> Self {
        Self {
            context,
            source,
            geo,
            version: AtomicU64::new(0),
            started: Instant::now(),
            probe_cache: Mutex::new(HashMap::new()),
        }
    }

    async fn reload(&self) -> Result<Value, IpcError> {
        let bytes = reload::fetch(&self.source)
            .await
            .map_err(|error| IpcError::Remote {
                code: "fetch_failed".to_owned(),
                message: error.to_string(),
            })?;
        let router = reload::build_router(&bytes, Arc::clone(&self.geo)).map_err(|error| {
            IpcError::Remote {
                code: "validate_failed".to_owned(),
                message: error.to_string(),
            }
        })?;
        let rules = router.rule_count();
        self.context.replace_router(router);
        let version = self.version.fetch_add(1, Ordering::Relaxed) + 1;
        Ok(json!({ "version": version, "rules": rules }))
    }

    fn status_body(&self, detail: bool) -> Value {
        let mut body = json!({
            "rules": self.context.router().rule_count(),
            "version": self.version.load(Ordering::Relaxed),
            "uptime_s": self.started.elapsed().as_secs(),
        });
        if detail {
            body["fakeip_entries"] = json!(self.context.fakeip.len());
            body["source"] = json!(match &self.source {
                reload::Source::File(path) => format!("file:{}", path.display()),
                reload::Source::Url(_) => "url".to_owned(),
            });
        }
        body
    }

    /// 内核内部状态；不回显订阅地址与 token。
    fn metrics_body(&self) -> Value {
        let (total, active, up, down) = self.context.metrics.snapshot();
        json!({
            "uptime_s": self.started.elapsed().as_secs(),
            "rules": self.context.router().rule_count(),
            "version": self.version.load(Ordering::Relaxed),
            "fakeip_entries": self.context.fakeip.len(),
            "connections_total": total,
            "connections_active": active,
            "bytes_up": up,
            "bytes_down": down,
            "source_kind": match &self.source {
                reload::Source::File(_) => "file",
                reload::Source::Url(_) => "url",
            },
        })
    }

    /// 策略组与节点的可观测快照（不含节点地址与凭据）。
    fn nodes_body(&self) -> Result<Value, IpcError> {
        let mut snapshot = self.context.router().node_snapshot();
        if let Ok(cache) = self.probe_cache.lock() {
            for node in &mut snapshot.nodes {
                if let Some(outcome) = cache.get(&node.name) {
                    node.tcp_ok = Some(outcome.reachable);
                    node.tcp_rtt_ms = outcome.rtt_ms;
                    node.tcp_error = outcome.error_code.clone();
                }
            }
        }
        serde_json::to_value(snapshot).map_err(|error| IpcError::Remote {
            code: "serialize_failed".to_owned(),
            message: error.to_string(),
        })
    }

    /// 对所有**配置中已登记**的节点并发做 TCP 连接探测。
    ///
    /// 与 `node_probe` 的回环白名单不同：这里的目标来自 profile 本身，属于
    /// 显式调用触发的主动外连；响应只回显节点名，地址绝不离开内核。
    /// 结果仅作观测，不写入健康表（`url-test` 仍由经代理的 HTTP 探测驱动）。
    async fn probe_nodes(&self, params: Value) -> Result<Value, IpcError> {
        let params: ProbeNodesParams =
            serde_json::from_value(object_params(params)).map_err(|error| IpcError::Remote {
                code: ERR_INVALID_PARAMS.to_owned(),
                message: error.to_string(),
            })?;
        if params.timeout_ms == 0 || params.timeout_ms > MAX_PROBE_TIMEOUT_MS {
            return Err(IpcError::Remote {
                code: ERR_INVALID_PARAMS.to_owned(),
                message: format!("timeout_ms must be 1..={MAX_PROBE_TIMEOUT_MS}"),
            });
        }

        let deadline = Duration::from_millis(params.timeout_ms);
        let mut results: Vec<NodeProbeOutcome> =
            join_all(self.context.router().probe_targets().into_iter().map(
                |(name, address)| async move {
                    let started = Instant::now();
                    let outcome =
                        tokio::time::timeout(deadline, TcpStream::connect(address.as_str())).await;
                    let (reachable, rtt_ms, error_code) = match outcome {
                        Ok(Ok(stream)) => {
                            drop(stream);
                            (true, Some(started.elapsed().as_secs_f64() * 1000.0), None)
                        }
                        Ok(Err(_)) => (false, None, Some("connect_failed".to_owned())),
                        Err(_) => (false, None, Some("timeout".to_owned())),
                    };
                    tracing::info!(
                        node = %name,
                        reachable,
                        rtt_ms = ?rtt_ms,
                        error_code = ?error_code,
                        "node tcp probe"
                    );
                    NodeProbeOutcome {
                        name,
                        reachable,
                        rtt_ms,
                        error_code,
                    }
                },
            ))
            .await;
        results.sort_by(|left, right| left.name.cmp(&right.name));

        match self.probe_cache.lock() {
            Ok(mut cache) => {
                cache.clear();
                cache.extend(
                    results
                        .iter()
                        .map(|outcome| (outcome.name.clone(), outcome.clone())),
                );
            }
            Err(error) => tracing::warn!(%error, "probe cache poisoned"),
        }
        serde_json::to_value(ProbeNodesResult { results }).map_err(|error| IpcError::Remote {
            code: "serialize_failed".to_owned(),
            message: error.to_string(),
        })
    }

    /// 手动锁定策略组成员，或恢复自动策略。
    fn select_node(&self, params: Value) -> Result<Value, IpcError> {
        let params: SelectNodeParams =
            serde_json::from_value(params).map_err(|error| IpcError::Remote {
                code: ERR_INVALID_PARAMS.to_owned(),
                message: error.to_string(),
            })?;
        let router = self.context.router();
        if let Err(error) = router.set_selection(&params.group, params.node.as_deref()) {
            let code = match error {
                SelectionError::UnknownGroup => ERR_UNKNOWN_GROUP,
                SelectionError::UnknownNode => ERR_UNKNOWN_NODE,
                SelectionError::Poisoned => "handler_error",
            };
            return Err(IpcError::Remote {
                code: code.to_owned(),
                message: format!("select_node rejected: {error:?}"),
            });
        }
        let view = router
            .node_snapshot()
            .groups
            .into_iter()
            .find(|group| group.name == params.group)
            .ok_or_else(|| IpcError::Remote {
                code: ERR_UNKNOWN_GROUP.to_owned(),
                message: "group vanished after selection".to_owned(),
            })?;
        tracing::info!(
            group = %params.group,
            mode = ?view.mode,
            selected = %view.selected,
            "node selection changed"
        );
        serde_json::to_value(SelectNodeResult {
            group: params.group,
            mode: view.mode,
            selected: view.selected,
        })
        .map_err(|error| IpcError::Remote {
            code: "serialize_failed".to_owned(),
            message: error.to_string(),
        })
    }

    /// 对指定 `host:port` 做 TCP 连接延迟探测。
    ///
    /// 当前仅允许回环目标，避免 IPC 成为任意内网探测入口；C1 后续里程碑会
    /// 扩展到配置中已登记的节点。探测失败是正常结果，不作为 IPC 错误返回。
    async fn node_probe(&self, params: Value) -> Result<Value, IpcError> {
        let params: NodeProbeParams =
            serde_json::from_value(params).map_err(|error| IpcError::Remote {
                code: ERR_INVALID_PARAMS.to_owned(),
                message: error.to_string(),
            })?;
        if params.timeout_ms == 0 || params.timeout_ms > MAX_PROBE_TIMEOUT_MS {
            return Err(IpcError::Remote {
                code: ERR_INVALID_PARAMS.to_owned(),
                message: format!("timeout_ms must be 1..={MAX_PROBE_TIMEOUT_MS}"),
            });
        }
        if !is_loopback_target(&params.address) {
            return Err(IpcError::Remote {
                code: ERR_FORBIDDEN_TARGET.to_owned(),
                message: "node_probe currently allows loopback targets only".to_owned(),
            });
        }

        let address = params.address;
        let started = Instant::now();
        let outcome = tokio::time::timeout(
            Duration::from_millis(params.timeout_ms),
            TcpStream::connect(address.as_str()),
        )
        .await;
        let result = match outcome {
            Ok(Ok(stream)) => {
                drop(stream);
                NodeProbeResult {
                    address,
                    reachable: true,
                    rtt_ms: Some(started.elapsed().as_secs_f64() * 1000.0),
                    error_code: None,
                }
            }
            Ok(Err(_)) => NodeProbeResult::unreachable(address, "connect_failed"),
            Err(_) => NodeProbeResult::unreachable(address, "timeout"),
        };
        serde_json::to_value(result).map_err(|error| IpcError::Remote {
            code: "serialize_failed".to_owned(),
            message: error.to_string(),
        })
    }
}

/// 无参方法允许调用方省略 `params`（`Value::Null`）。
fn object_params(params: Value) -> Value {
    if params.is_null() { json!({}) } else { params }
}

/// 事件发出时刻（Unix 微秒），供控制面测量端到端延迟。
fn now_us() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |value| value.as_micros() as u64)
}

/// 把处理器错误拆成事件载荷需要的 `code` / `message`。
fn error_parts(error: &IpcError) -> (String, String) {
    match error {
        IpcError::Remote { code, message } => (code.clone(), message.clone()),
        other => ("handler_error".to_owned(), other.to_string()),
    }
}

#[async_trait]
impl Handler for KernelHandler {
    async fn handle(&self, method: &str, params: Value) -> Result<Value, IpcError> {
        match method {
            METHOD_STATUS => {
                let detail = params
                    .get("detail")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                Ok(self.status_body(detail))
            }
            METHOD_RELOAD => self.reload().await,
            METHOD_METRICS => Ok(self.metrics_body()),
            METHOD_NODE_PROBE => self.node_probe(params).await,
            METHOD_NODES => self.nodes_body(),
            METHOD_PROBE_NODES => self.probe_nodes(params).await,
            METHOD_SELECT_NODE => self.select_node(params),
            other => Err(IpcError::Remote {
                code: ERR_UNKNOWN_METHOD.to_owned(),
                message: format!("unknown method: {other}"),
            }),
        }
    }

    /// `reload` 的结果以事件形式推送后，再返回同一份响应。
    ///
    /// 事件先入队、响应后入队，由单一 writer 串行写出，因此客户端总是先看到事件；
    /// 事件推送失败（队列满/连接关闭）不影响请求本身的成败。
    async fn handle_with(
        &self,
        method: &str,
        params: Value,
        events: &EventSink,
    ) -> Result<Value, IpcError> {
        if method != METHOD_RELOAD {
            return self.handle(method, params).await;
        }

        match self.reload().await {
            Ok(value) => {
                events.send(
                    "reload",
                    json!({
                        "state": "applied",
                        "version": value.get("version").cloned().unwrap_or(Value::Null),
                        "rules": value.get("rules").cloned().unwrap_or(Value::Null),
                        "ts_us": now_us(),
                    }),
                );
                Ok(value)
            }
            Err(error) => {
                let (code, message) = error_parts(&error);
                events.send(
                    "reload",
                    json!({
                        "state": "rejected",
                        "code": code,
                        "message": message,
                        "ts_us": now_us(),
                    }),
                );
                Err(error)
            }
        }
    }
}

/// 启动 IPC 服务。仅 Windows 实现；其他平台记录告警并忽略。
#[cfg(windows)]
pub fn spawn(pipe: String, context: Arc<AppContext>, source: reload::Source, geo: Arc<GeoData>) {
    let handler = Arc::new(KernelHandler::new(context, source, geo));
    tracing::info!(pipe = %pipe, "ipc server listening");
    drop(tokio::spawn(async move {
        if let Err(error) = client_sidecar::ipc::pipe::serve(&pipe, handler).await {
            tracing::error!(%error, "ipc server stopped");
        }
    }));
}

#[cfg(not(windows))]
pub fn spawn(
    _pipe: String,
    _context: Arc<AppContext>,
    _source: reload::Source,
    _geo: Arc<GeoData>,
) {
    tracing::warn!("--ipc-pipe is only supported on Windows");
}
