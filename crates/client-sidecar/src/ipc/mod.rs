//! IPC 抽象层。

#[cfg(windows)]
pub mod acl;
#[cfg(windows)]
pub mod pipe;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use serde_json::Value;

use crate::{
    error::IpcError,
    protocol::{Message, VERSION},
};

/// 事件流：有界广播，慢消费者会丢弃最旧事件（由 `broadcast` 语义保证）。
pub type EventStream = tokio::sync::broadcast::Receiver<Message>;

/// 绑定到单条连接的出站事件通道。
///
/// 队列满时不阻塞数据面，而是丢弃并计数（ADR 0002 §1「有界队列，满时丢弃最旧并计数」）。
#[derive(Clone)]
pub struct EventSink {
    tx: tokio::sync::mpsc::Sender<Message>,
    dropped: Arc<AtomicU64>,
}

impl EventSink {
    pub(crate) fn new(tx: tokio::sync::mpsc::Sender<Message>) -> Self {
        Self {
            tx,
            dropped: Arc::new(AtomicU64::new(0)),
        }
    }

    /// 尝试投递事件；队列满或连接已关闭时返回 `false`。
    pub fn send(&self, kind: &str, payload: Value) -> bool {
        let message = Message::Event {
            v: VERSION,
            kind: kind.to_owned(),
            payload,
        };
        match self.tx.try_send(message) {
            Ok(()) => true,
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                false
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => false,
        }
    }

    /// 因队列满而丢弃的事件数。
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// 传输层抽象。业务层只依赖此 trait，不感知管道或未来的共享内存实现。
#[async_trait]
pub trait IpcTransport: Send + Sync {
    /// 发送请求并等待响应；超时由实现方保证。
    async fn request(&self, method: &str, params: Value) -> Result<Value, IpcError>;
    /// 订阅服务端事件。
    fn events(&self) -> EventStream;
}

/// 服务端方法处理器。
#[async_trait]
pub trait Handler: Send + Sync {
    async fn handle(&self, method: &str, params: Value) -> Result<Value, IpcError>;

    /// 带事件通道的处理入口；默认实现忽略事件通道，保持既有处理器无需改动。
    ///
    /// 需要推送 `applied` / `rejected` 等异步通知的处理器覆盖此方法。
    async fn handle_with(
        &self,
        method: &str,
        params: Value,
        _events: &EventSink,
    ) -> Result<Value, IpcError> {
        self.handle(method, params).await
    }
}
