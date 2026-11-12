//! `client-sidecar`：控制面与内核之间的 IPC 层。
//!
//! S1 范围：帧协议、`IpcTransport` / `Handler` trait、命名管道实现。
//! 业务方法（status/reload）在 S2 接入内核，快照管理在 S3。

pub mod contract;
pub mod error;
pub mod fetch;
pub mod ipc;
pub mod logging;
pub mod protocol;
pub mod snapshot;

pub use error::IpcError;
pub use ipc::{EventSink, Handler, IpcTransport};
pub use protocol::{ErrorBody, MAX_FRAME, Message, VERSION};
