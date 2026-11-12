//! 后台 IPC 任务：独立的 tokio 线程持有到内核的单连接。
//!
//! 职责：连接（指数退避）→ 1 Hz 轮询 `metrics` → 转发内核事件 → 执行
//! UI 发来的 `node_probe` 命令。状态变化时调用 `egui::Context::request_repaint`
//! 唤醒 UI 线程按需重绘（事件驱动，无空转渲染）。
//!
//! 退出：UI 关闭窗口时 `TaskHandle` 落盘 Drop（或显式 `stop()`），任务在
//! select 的 watch 分支感知后结束；进程退出本身也会回收线程。

use std::sync::Arc;

use tokio::sync::{mpsc, watch};

use crate::state::{Command, SharedState};

#[cfg(windows)]
use std::time::Duration;

#[cfg(windows)]
use client_sidecar::IpcError;
#[cfg(windows)]
use client_sidecar::IpcTransport;
#[cfg(windows)]
use client_sidecar::contract::{
    METHOD_METRICS, METHOD_NODE_PROBE, Metrics, NodeProbeParams, NodeProbeResult,
};
#[cfg(windows)]
use client_sidecar::ipc::pipe::PipeClient;
#[cfg(windows)]
use client_sidecar::protocol::Message;
#[cfg(windows)]
use serde_json::Value;

#[cfg(windows)]
const MAX_BACKOFF_MS: u64 = 5_000;
/// UI 请求超时上限（`PipeClient::request` 内部为 3 s）。
#[cfg(windows)]
const REQUEST_TIMEOUT_MS: u64 = 2_000;
/// 退避起点。
const INITIAL_BACKOFF_MS: u64 = 100;

/// 任务停止句柄：UI Drop 时自动触发，也可显式调用 [`TaskHandle::stop`]。
pub struct TaskHandle {
    shutdown: watch::Sender<bool>,
}

impl TaskHandle {
    /// 请求任务退出；不阻塞等待（进程退出会回收线程）。
    pub fn stop(&self) {
        let _ = self.shutdown.send(true);
    }
}

/// 在独立线程上启动 IPC 任务，返回 UI 侧的命令通道与停止句柄。
///
/// 非 Windows 平台没有管道传输层：写入一条提示事件并返回不可用通道
/// （探测命令静默失败），保证双平台可编译、CI 可跑。
pub fn spawn(
    pipe: String,
    state: Arc<SharedState>,
    ctx: eframe::egui::Context,
) -> (mpsc::UnboundedSender<Command>, TaskHandle) {
    let (commands, rx) = mpsc::unbounded_channel::<Command>();
    let (shutdown, shutdown_rx) = watch::channel(false);

    spawn_platform(pipe, state, ctx, rx, shutdown_rx);

    (commands, TaskHandle { shutdown })
}

#[cfg(windows)]
fn spawn_platform(
    pipe: String,
    state: Arc<SharedState>,
    ctx: eframe::egui::Context,
    rx: mpsc::UnboundedReceiver<Command>,
    shutdown: watch::Receiver<bool>,
) {
    let error_state = Arc::clone(&state);
    let error_ctx = ctx.clone();
    let spawned = std::thread::Builder::new()
        .name("ipc-task".to_owned())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    state.update(|s| s.set_disconnected(format!("runtime: {error}")));
                    ctx.request_repaint();
                    return;
                }
            };
            runtime.block_on(supervise(pipe, state, ctx, rx, shutdown));
        });
    if let Err(error) = spawned {
        error_state.update(|s| s.set_disconnected(format!("thread spawn: {error}")));
        error_ctx.request_repaint();
    }
}

#[cfg(windows)]
async fn supervise(
    pipe: String,
    state: Arc<SharedState>,
    ctx: eframe::egui::Context,
    mut rx: mpsc::UnboundedReceiver<Command>,
    mut shutdown: watch::Receiver<bool>,
) {
    let poll = Duration::from_secs(1);
    let mut backoff_ms = INITIAL_BACKOFF_MS;

    loop {
        if *shutdown.borrow() {
            break;
        }
        match PipeClient::connect(&pipe).await {
            Ok(client) => {
                backoff_ms = INITIAL_BACKOFF_MS;
                state.update(|s| s.set_connected());
                ctx.request_repaint();
                tracing::info!(pipe = %pipe, "kernel ipc connected");

                let error = session(&client, &state, &ctx, &mut rx, &mut shutdown, poll).await;
                client.close().await;
                if *shutdown.borrow() {
                    break;
                }
                state.update(|s| s.set_disconnected(error.clone()));
                ctx.request_repaint();
                tracing::warn!(%error, "kernel ipc session ended");
                tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                backoff_ms = (backoff_ms * 2).min(MAX_BACKOFF_MS);
            }
            Err(error) => {
                state.update(|s| s.set_connect_failed(error.to_string()));
                ctx.request_repaint();
                tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                backoff_ms = (backoff_ms * 2).min(MAX_BACKOFF_MS);
            }
        }
    }
    tracing::info!("ipc task stopped");
}

/// 单次已连接会话：轮询 metrics、转发事件、执行探测，直到连接层失败或关停。
///
/// 内核业务层拒绝（`Remote` 错误码）不结束会话：metrics 轮询记 warn，
/// 探测拒绝转成结果呈现给 UI。
#[cfg(windows)]
async fn session(
    client: &PipeClient,
    state: &Arc<SharedState>,
    ctx: &eframe::egui::Context,
    rx: &mut mpsc::UnboundedReceiver<Command>,
    shutdown: &mut watch::Receiver<bool>,
    poll: Duration,
) -> String {
    let mut events = client.events();
    let mut ticker = tokio::time::interval(poll);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // interval 首个 tick 立即返回，跳过后按周期对齐。
    let _ = ticker.tick().await;

    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return "stopped".to_owned();
                }
            }
            command = rx.recv() => {
                match command {
                    Some(Command::Probe { address, timeout_ms }) => {
                        if let Err(error) = run_probe(client, state, ctx, &address, timeout_ms).await {
                            if is_connection_error(&error) {
                                return error.to_string();
                            }
                            tracing::warn!(%error, "probe command failed");
                        }
                    }
                    None => return "command channel closed".to_owned(),
                }
            }
            _ = ticker.tick() => {
                if let Err(error) = poll_metrics(client, state).await {
                    if is_connection_error(&error) {
                        return error.to_string();
                    }
                    tracing::warn!(%error, "metrics poll failed");
                }
                ctx.request_repaint();
            }
            event = events.recv() => {
                match event {
                    Ok(Message::Event { kind, payload, .. }) => {
                        state.update(|s| s.apply_event(&kind, &payload));
                        ctx.request_repaint();
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(dropped)) => {
                        tracing::warn!(dropped, "gui events dropped");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        return "event stream closed".to_owned();
                    }
                    Ok(_) => {}
                }
            }
        }
    }
}

/// 轮询一轮 `metrics`；解析失败只记日志，不断连接。
#[cfg(windows)]
async fn poll_metrics(client: &PipeClient, state: &Arc<SharedState>) -> Result<(), IpcError> {
    let value = client.request(METHOD_METRICS, Value::Null).await?;
    match serde_json::from_value::<Metrics>(value) {
        Ok(metrics) => {
            state.update(|s| s.apply_metrics(metrics));
        }
        Err(error) => tracing::warn!(%error, "metrics parse failed"),
    }
    Ok(())
}

/// 执行一次探测；内核拒绝（参数/目标越界）也作为结果呈现给 UI。
#[cfg(windows)]
async fn run_probe(
    client: &PipeClient,
    state: &Arc<SharedState>,
    ctx: &eframe::egui::Context,
    address: &str,
    timeout_ms: u64,
) -> Result<(), IpcError> {
    let params = serde_json::to_value(NodeProbeParams {
        address: address.to_owned(),
        timeout_ms: timeout_ms.min(REQUEST_TIMEOUT_MS),
    })
    .map_err(|_| IpcError::Unexpected)?;

    let parsed = match client.request(METHOD_NODE_PROBE, params).await {
        Ok(value) => serde_json::from_value::<NodeProbeResult>(value),
        Err(IpcError::Remote { code, message }) => {
            tracing::warn!(code = %code, %message, "probe rejected by kernel");
            Ok(NodeProbeResult::unreachable(address.to_owned(), &code))
        }
        Err(other) => return Err(other),
    };
    let result = parsed.unwrap_or_else(|error| {
        tracing::warn!(%error, "probe result parse failed");
        NodeProbeResult::unreachable(address.to_owned(), "serialize_failed")
    });

    state.update(|s| s.push_probe(result));
    ctx.request_repaint();
    Ok(())
}

/// 连接类错误才需要重建连接；内核拒绝（`Remote`）视为正常业务结果。
#[cfg(windows)]
fn is_connection_error(error: &IpcError) -> bool {
    matches!(
        error,
        IpcError::Io(_)
            | IpcError::Closed
            | IpcError::Timeout
            | IpcError::Protocol(_)
            | IpcError::Unexpected
    )
}

#[cfg(not(windows))]
fn spawn_platform(
    _pipe: String,
    state: Arc<SharedState>,
    ctx: eframe::egui::Context,
    _rx: mpsc::UnboundedReceiver<Command>,
    _shutdown: watch::Receiver<bool>,
) {
    state.update(|s| {
        s.set_disconnected("ipc transport requires Windows named pipe".to_owned());
        s.apply_event(
            "gui",
            &serde_json::json!({ "note": "named-pipe transport is Windows only" }),
        );
    });
    ctx.request_repaint();
}

#[cfg(all(test, windows))]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use client_sidecar::EventSink;
    use client_sidecar::contract::{Metrics, NodeProbeParams, NodeProbeResult};
    use client_sidecar::ipc::pipe::{PipeClient, serve};
    use serde_json::{Value, json};

    use super::*;
    use crate::state::LinkStatus;

    /// 桩内核：`metrics` 返回固定值并附带一条 reload 事件；`node_probe` 回显可达结果。
    struct StubKernel {
        metrics: Metrics,
        rtt_ms: f64,
    }

    #[async_trait::async_trait]
    impl client_sidecar::Handler for StubKernel {
        async fn handle(&self, method: &str, params: Value) -> Result<Value, IpcError> {
            match method {
                METHOD_METRICS => {
                    serde_json::to_value(&self.metrics).map_err(|_| IpcError::Unexpected)
                }
                METHOD_NODE_PROBE => {
                    let params: NodeProbeParams =
                        serde_json::from_value(params).map_err(|error| IpcError::Remote {
                            code: "invalid_params".to_owned(),
                            message: error.to_string(),
                        })?;
                    let result = NodeProbeResult {
                        address: params.address,
                        reachable: true,
                        rtt_ms: Some(self.rtt_ms),
                        error_code: None,
                    };
                    serde_json::to_value(result).map_err(|_| IpcError::Unexpected)
                }
                _ => Ok(Value::Null),
            }
        }

        async fn handle_with(
            &self,
            method: &str,
            params: Value,
            events: &EventSink,
        ) -> Result<Value, IpcError> {
            if method == METHOD_METRICS {
                // 顺带推一条事件，验证 GUI 侧事件转发链路。
                events.send("reload", json!({ "state": "applied", "version": 3 }));
            }
            self.handle(method, params).await
        }
    }

    fn pipe_name(tag: &str) -> String {
        format!(r"\\.\pipe\mcloud-gui-test-{}-{tag}", std::process::id())
    }

    async fn wait_until(timeout_s: u64, condition: impl Fn() -> bool, what: &str) {
        let deadline = Instant::now() + Duration::from_secs(timeout_s);
        while !condition() {
            assert!(Instant::now() < deadline, "timeout waiting for {what}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// 重试连接（serve 在两次连接之间会短暂重建管道实例）。
    async fn connect_with_retry(name: &str) -> PipeClient {
        for _ in 0..400 {
            if let Ok(client) = PipeClient::connect(name).await {
                return client;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("pipe never became available: {name}");
    }

    #[tokio::test]
    async fn task_end_to_end_metrics_events_and_probe() {
        let name = pipe_name("e2e");
        let metrics = Metrics {
            uptime_s: 42,
            rules: 5,
            version: 3,
            fakeip_entries: 2,
            connections_total: 9,
            connections_active: 1,
            bytes_up: 256,
            bytes_down: 2 * 1_048_576,
            source_kind: "file".to_owned(),
        };
        let handler = Arc::new(StubKernel {
            metrics: metrics.clone(),
            rtt_ms: 0.42,
        });

        // serve：第一条连接结束后重建管道实例，供「释放验证」复用。
        let server = tokio::spawn({
            let name = name.clone();
            let handler = Arc::clone(&handler);
            async move { serve(&name, handler).await }
        });

        let state = SharedState::new();
        let ctx = eframe::egui::Context::default();
        let (commands, handle) = spawn(name.clone(), state.clone(), ctx);

        // 连接 + 首轮 metrics（1 s 轮询）。
        wait_until(6, || state.snapshot().kernel.is_some(), "metrics").await;
        let snapshot = state.snapshot();
        assert_eq!(snapshot.link.status, LinkStatus::Connected);
        assert_eq!(snapshot.kernel, Some(metrics));

        // 事件转发：metrics 轮询触发 stub 推送 reload 事件。
        wait_until(
            6,
            || !state.snapshot().events_recent_first().is_empty(),
            "event",
        )
        .await;
        let events = state.snapshot().events_recent_first();
        assert_eq!(events[0].kind, "reload");
        assert_eq!(events[0].summary, "applied v3");

        // 探测命令：往返 stub 后进入结果环（stub 回显请求地址）。
        commands
            .send(Command::probe("localhost:7911"))
            .expect("send probe");
        wait_until(
            6,
            || !state.snapshot().probes_recent_first().is_empty(),
            "probe",
        )
        .await;
        let probes = state.snapshot().probes_recent_first();
        assert_eq!(probes[0].address, "localhost:7911");
        assert!(probes[0].reachable);
        assert!((probes[0].rtt_ms.unwrap_or(-1.0) - 0.42).abs() < f64::EPSILON);

        // 停止后连接应被释放：第二个客户端可以接入（单连接模型语义）。
        handle.stop();
        let client = connect_with_retry(&name).await;
        client.close().await;

        server.abort();
    }

    #[tokio::test]
    async fn probe_rejection_is_presented_not_fatal() {
        struct RejectingKernel;

        #[async_trait::async_trait]
        impl client_sidecar::Handler for RejectingKernel {
            async fn handle(&self, _method: &str, _params: Value) -> Result<Value, IpcError> {
                Err(IpcError::Remote {
                    code: "forbidden_target".to_owned(),
                    message: "loopback only".to_owned(),
                })
            }
        }

        let name = pipe_name("rej");
        let handler = Arc::new(RejectingKernel);
        let server = tokio::spawn({
            let name = name.clone();
            let handler = Arc::clone(&handler);
            async move { serve(&name, handler).await }
        });

        let state = SharedState::new();
        let ctx = eframe::egui::Context::default();
        let (commands, handle) = spawn(name, state.clone(), ctx);

        wait_until(6, || state.snapshot().link.connects > 0, "connect").await;
        commands
            .send(Command::probe("10.0.0.1:80"))
            .expect("send probe");
        wait_until(
            6,
            || !state.snapshot().probes_recent_first().is_empty(),
            "probe",
        )
        .await;

        // 拒绝作为结果呈现，会话保持连接。
        let probes = state.snapshot().probes_recent_first();
        assert!(!probes[0].reachable);
        assert_eq!(probes[0].error_code.as_deref(), Some("forbidden_target"));
        assert_eq!(state.snapshot().link.status, LinkStatus::Connected);

        handle.stop();
        server.abort();
    }
}
