//! Windows 命名管道传输实现。
//!
//! 服务端用 SDDL 限制为 `SYSTEM` + 当前用户；客户端走同一路径连接。
//! 单连接模型：一次 `accept_once` 处理一条连接，断开后由 `serve` 重新创建实例。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, WriteHalf};
use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
};
use tokio::sync::{Mutex, broadcast, mpsc, oneshot, watch};

use crate::error::IpcError;
use crate::ipc::acl::PipeSecurity;
use crate::ipc::{EventSink, EventStream, Handler, IpcTransport};
use crate::protocol::{self, ErrorBody, Message, VERSION};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
const EVENT_BUFFER: usize = 256;

pub struct PipeClient {
    writer: Mutex<WriteHalf<NamedPipeClient>>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Message>>>>,
    events: broadcast::Sender<Message>,
    next_id: AtomicU64,
    shutdown: watch::Sender<bool>,
    reader: Option<tokio::task::JoinHandle<()>>,
}

impl PipeClient {
    pub async fn connect(name: &str) -> Result<Self, IpcError> {
        let stream = ClientOptions::new().open(name)?;
        let (mut reader, writer) = tokio::io::split(stream);
        let pending = Arc::new(Mutex::new(HashMap::<u64, oneshot::Sender<Message>>::new()));
        let (events, _) = broadcast::channel(EVENT_BUFFER);
        let (shutdown, mut shutdown_rx) = watch::channel(false);

        let pending_task = Arc::clone(&pending);
        let events_task = events.clone();
        let reader_task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = shutdown_rx.changed() => break,
                    result = protocol::read_frame(&mut reader) => {
                        match result {
                            Ok(message @ Message::Response { id, .. }) => {
                                if let Some(sender) = pending_task.lock().await.remove(&id) {
                                    let _ = sender.send(message);
                                }
                            }
                            Ok(message @ Message::Event { .. }) => {
                                let _ = events_task.send(message);
                            }
                            Ok(Message::Request { .. }) => {}
                            Err(_) => break,
                        }
                    }
                }
            }
            // 连接断开或主动关闭：唤醒所有等待者，避免请求永久挂起。
            pending_task.lock().await.clear();
        });

        Ok(Self {
            writer: Mutex::new(writer),
            pending,
            events,
            next_id: AtomicU64::new(1),
            shutdown,
            reader: Some(reader_task),
        })
    }

    /// 关闭写端并等待读任务退出；服务端会因此读到 EOF。
    pub async fn close(mut self) {
        let _ = self.shutdown.send(true);
        {
            let mut writer = self.writer.lock().await;
            let _ = writer.shutdown().await;
        }
        if let Some(handle) = self.reader.take() {
            let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
        }
    }
}

impl Drop for PipeClient {
    fn drop(&mut self) {
        // 未显式 close 时也通知读任务退出，避免任务泄漏。
        let _ = self.shutdown.send(true);
    }
}

#[async_trait]
impl IpcTransport for PipeClient {
    async fn request(&self, method: &str, params: Value) -> Result<Value, IpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().await.insert(id, sender);

        let message = Message::Request {
            v: VERSION,
            id,
            method: method.to_owned(),
            params,
        };
        {
            let mut writer = self.writer.lock().await;
            protocol::write_frame(&mut *writer, &message).await?;
        }

        let reply = tokio::time::timeout(REQUEST_TIMEOUT, receiver)
            .await
            .map_err(|_| IpcError::Timeout)??;

        match reply {
            Message::Response {
                ok: true, result, ..
            } => Ok(result.unwrap_or(Value::Null)),
            Message::Response {
                error: Some(error), ..
            } => Err(IpcError::Remote {
                code: error.code,
                message: error.message,
            }),
            _ => Err(IpcError::Unexpected),
        }
    }

    fn events(&self) -> EventStream {
        self.events.subscribe()
    }
}

/// 接受一条连接、处理到断开后返回。供测试与单次调用使用。
pub async fn accept_once<H>(name: &str, handler: Arc<H>) -> Result<(), IpcError>
where
    H: Handler + 'static,
{
    let server = create_server(name, true)?;
    server.connect().await?;
    let (reader, writer) = tokio::io::split(server);
    run_connection(reader, writer, handler).await
}

/// 循环接受连接；每次断开后重建管道实例。
pub async fn serve<H>(name: &str, handler: Arc<H>) -> Result<(), IpcError>
where
    H: Handler + 'static,
{
    let mut first = true;
    loop {
        let server = create_server(name, first)?;
        first = false;
        server.connect().await?;
        let (reader, writer) = tokio::io::split(server);
        if let Err(error) = run_connection(reader, writer, Arc::clone(&handler)).await {
            tracing::warn!(%error, "ipc connection ended with error");
        }
    }
}

async fn run_connection<R, W, H>(
    mut reader: R,
    mut writer: W,
    handler: Arc<H>,
) -> Result<(), IpcError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
    H: Handler,
{
    // 响应与事件共用同一条 writer，由独立任务串行写出：
    // 避免在 `select!` 里取消到一半的 `write_frame`，造成帧错位。
    let (tx, mut rx) = mpsc::channel::<Message>(EVENT_BUFFER);
    let events = EventSink::new(tx.clone());
    let writer_task = tokio::spawn(async move {
        while let Some(message) = rx.recv().await {
            if protocol::write_frame(&mut writer, &message).await.is_err() {
                break;
            }
        }
    });

    let result = loop {
        let message = match protocol::read_frame(&mut reader).await {
            Ok(message) => message,
            Err(protocol::ProtocolError::Io(error))
                if error.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break Ok(());
            }
            Err(error) => break Err(error.into()),
        };

        let Message::Request {
            v,
            id,
            method,
            params,
        } = message
        else {
            continue;
        };

        let response = build_response(v, id, &method, params, handler.as_ref(), &events).await;
        if tx.send(response).await.is_err() {
            break Err(IpcError::Closed);
        }
    };

    drop(tx);
    drop(events);
    let _ = writer_task.await;
    result
}

async fn build_response<H>(
    v: u16,
    id: u64,
    method: &str,
    params: Value,
    handler: &H,
    events: &EventSink,
) -> Message
where
    H: Handler,
{
    if v != VERSION {
        return Message::Response {
            v: VERSION,
            id,
            ok: false,
            result: None,
            error: Some(ErrorBody {
                code: "version_mismatch".to_owned(),
                message: format!("expected {VERSION}, got {v}"),
            }),
        };
    }

    match handler.handle_with(method, params, events).await {
        Ok(value) => Message::Response {
            v: VERSION,
            id,
            ok: true,
            result: Some(value),
            error: None,
        },
        Err(error) => Message::Response {
            v: VERSION,
            id,
            ok: false,
            result: None,
            error: Some(error_body(error)),
        },
    }
}

/// 保留处理器给出的错误码（如 `fetch_failed` / `validate_failed`），其余归为 `handler_error`。
fn error_body(error: IpcError) -> ErrorBody {
    match error {
        IpcError::Remote { code, message } => ErrorBody { code, message },
        other => ErrorBody {
            code: "handler_error".to_owned(),
            message: other.to_string(),
        },
    }
}

// 单一授权点：仅此处构造并传入安全描述符。
#[allow(unsafe_code)]
fn create_server(name: &str, first_instance: bool) -> Result<NamedPipeServer, IpcError> {
    let mut options = ServerOptions::new();
    options.first_pipe_instance(first_instance);
    let mut security = PipeSecurity::for_current_user()?;
    // Safety: `security` 在调用期间存活，指针指向有效的 SECURITY_ATTRIBUTES。
    let server =
        unsafe { options.create_with_security_attributes_raw(name, security.as_mut_ptr())? };
    Ok(server)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo;

    #[async_trait]
    impl Handler for Echo {
        async fn handle(&self, method: &str, params: Value) -> Result<Value, IpcError> {
            Ok(serde_json::json!({ "method": method, "params": params }))
        }
    }

    fn pipe_name(tag: &str) -> String {
        format!(r"\\.\pipe\mcloud-test-{}-{tag}", std::process::id())
    }

    async fn connect_with_retry(name: &str) -> PipeClient {
        for _ in 0..100 {
            if let Ok(client) = PipeClient::connect(name).await {
                return client;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("pipe never became available: {name}");
    }

    #[tokio::test]
    async fn request_response_roundtrip() {
        let name = pipe_name("rr");
        let server = tokio::spawn({
            let name = name.clone();
            async move { accept_once(&name, Arc::new(Echo)).await }
        });

        let client = connect_with_retry(&name).await;
        let result = client
            .request("status", serde_json::json!({"detail": true}))
            .await
            .expect("request");
        assert_eq!(result["method"], "status");
        assert_eq!(result["params"]["detail"], true);
        client.close().await;
        server.await.expect("join").expect("server");
    }

    struct Emitter;

    #[async_trait]
    impl Handler for Emitter {
        async fn handle(&self, _method: &str, _params: Value) -> Result<Value, IpcError> {
            Ok(Value::Null)
        }

        async fn handle_with(
            &self,
            _method: &str,
            _params: Value,
            events: &EventSink,
        ) -> Result<Value, IpcError> {
            assert!(events.send(
                "reload",
                serde_json::json!({"state": "applied", "version": 1})
            ));
            Ok(serde_json::json!({"ok": true}))
        }
    }

    #[tokio::test]
    async fn handler_can_push_event_before_response() {
        let name = pipe_name("evt");
        let server = tokio::spawn({
            let name = name.clone();
            async move { accept_once(&name, Arc::new(Emitter)).await }
        });

        let client = connect_with_retry(&name).await;
        // 必须在请求前订阅，否则广播会错过事件。
        let mut events = client.events();
        let result = client
            .request("reload", Value::Null)
            .await
            .expect("request");
        assert_eq!(result["ok"], true);

        let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("event timeout")
            .expect("event");
        match event {
            Message::Event { kind, payload, .. } => {
                assert_eq!(kind, "reload");
                assert_eq!(payload["state"], "applied");
                assert_eq!(payload["version"], 1);
            }
            other => panic!("unexpected message: {other:?}"),
        }

        client.close().await;
        server.await.expect("join").expect("server");
    }

    struct Rejecting;

    #[async_trait]
    impl Handler for Rejecting {
        async fn handle(&self, _method: &str, _params: Value) -> Result<Value, IpcError> {
            Err(IpcError::Unsupported)
        }

        async fn handle_with(
            &self,
            _method: &str,
            _params: Value,
            events: &EventSink,
        ) -> Result<Value, IpcError> {
            assert!(events.send(
                "reload",
                serde_json::json!({"state": "rejected", "code": "validate_failed"})
            ));
            Err(IpcError::Remote {
                code: "validate_failed".to_owned(),
                message: "rules is empty".to_owned(),
            })
        }
    }

    #[tokio::test]
    async fn rejected_event_and_error_response_coexist() {
        let name = pipe_name("rej");
        let server = tokio::spawn({
            let name = name.clone();
            async move { accept_once(&name, Arc::new(Rejecting)).await }
        });

        let client = connect_with_retry(&name).await;
        let mut events = client.events();
        let error = client
            .request("reload", Value::Null)
            .await
            .expect_err("should fail");
        match error {
            IpcError::Remote { code, .. } => assert_eq!(code, "validate_failed"),
            other => panic!("unexpected error: {other}"),
        }

        let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("event timeout")
            .expect("event");
        match event {
            Message::Event { payload, .. } => {
                assert_eq!(payload["state"], "rejected");
                assert_eq!(payload["code"], "validate_failed");
            }
            other => panic!("unexpected message: {other:?}"),
        }

        client.close().await;
        server.await.expect("join").expect("server");
    }

    #[tokio::test]
    async fn event_sink_drops_and_counts_when_queue_is_full() {
        let (tx, mut rx) = mpsc::channel::<Message>(1);
        let sink = EventSink::new(tx);
        assert!(sink.send("first", Value::Null));
        assert!(!sink.send("second", Value::Null));
        assert_eq!(sink.dropped(), 1);

        let first = rx.recv().await.expect("first");
        assert!(matches!(first, Message::Event { .. }));
    }

    #[tokio::test]
    async fn handler_error_is_returned_as_remote_error() {
        struct Failing;

        #[async_trait]
        impl Handler for Failing {
            async fn handle(&self, _method: &str, _params: Value) -> Result<Value, IpcError> {
                Err(IpcError::Unsupported)
            }
        }

        let name = pipe_name("err");
        let server = tokio::spawn({
            let name = name.clone();
            async move { accept_once(&name, Arc::new(Failing)).await }
        });

        let client = connect_with_retry(&name).await;
        let error = client
            .request("nope", Value::Null)
            .await
            .expect_err("should fail");
        match error {
            IpcError::Remote { code, .. } => assert_eq!(code, "handler_error"),
            other => panic!("unexpected error: {other}"),
        }
        client.close().await;
        server.await.expect("join").expect("server");
    }
}
