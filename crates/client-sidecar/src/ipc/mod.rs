//! IPC 抽象层。

#[cfg(windows)]
pub mod acl;
#[cfg(windows)]
pub mod pipe;

use async_trait::async_trait;
use serde_json::Value;

use crate::{error::IpcError, protocol::Message};

/// 事件流：有界广播，慢消费者会丢弃最旧事件（由 `broadcast` 语义保证）。
pub type EventStream = tokio::sync::broadcast::Receiver<Message>;

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
}
