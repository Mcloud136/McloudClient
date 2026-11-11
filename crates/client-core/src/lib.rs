//! `client-core` 库入口。
//!
//! 数据面各模块在此对外暴露，供未来的 sidecar、集成测试与嵌入式调用复用。
//! 二进制入口仍在 `main.rs`，二者共享同一套模块，避免逻辑分叉。

pub mod app;
pub mod config;
pub mod context;
pub mod dns;
pub mod geo;
pub mod health;
pub mod ipc;
pub mod outbound;
pub mod profile;
pub mod relay;
pub mod reload;
pub mod router;
pub mod rules;
pub mod service;
pub mod socks5;
pub mod udp;

/// 文件日志与内核共用一套实现；保留 `client_core::logging` 路径供既有调用方使用。
pub use client_sidecar::logging;
