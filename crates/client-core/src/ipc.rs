//! 内核侧 IPC 方法处理。
//!
//! 提供 `status` 与 `reload`。`reload` 复用与定时重载相同的
//! 「先构建、后替换」路径，失败时保留现有路由器。

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use async_trait::async_trait;
use client_sidecar::{EventSink, Handler, IpcError};
use serde_json::{Value, json};

use crate::{context::AppContext, geo::GeoData, reload};

pub struct KernelHandler {
    context: Arc<AppContext>,
    source: reload::Source,
    geo: Arc<GeoData>,
    version: AtomicU64,
    started: Instant,
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
            "status" => {
                let detail = params
                    .get("detail")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                Ok(self.status_body(detail))
            }
            "reload" => self.reload().await,
            other => Err(IpcError::Remote {
                code: "unknown_method".to_owned(),
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
        if method != "reload" {
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
                    }),
                );
                Ok(value)
            }
            Err(error) => {
                let (code, message) = error_parts(&error);
                events.send(
                    "reload",
                    json!({ "state": "rejected", "code": code, "message": message }),
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
