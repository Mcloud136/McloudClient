//! IPC 错误类型。

use crate::protocol::ProtocolError;

#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("protocol: {0}")]
    Protocol(#[from] ProtocolError),

    #[error("transport closed")]
    Closed,

    #[error("request timed out")]
    Timeout,

    #[error("remote error {code}: {message}")]
    Remote { code: String, message: String },

    #[error("operation not supported on this platform")]
    Unsupported,

    #[error("unexpected message from peer")]
    Unexpected,

    #[error("security descriptor: {0}")]
    Security(String),
}

impl From<tokio::sync::oneshot::error::RecvError> for IpcError {
    fn from(_: tokio::sync::oneshot::error::RecvError) -> Self {
        Self::Closed
    }
}
