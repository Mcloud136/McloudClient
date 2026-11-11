//! `client-core` 库入口。
//!
//! 数据面各模块在此对外暴露，供未来的 sidecar、集成测试与嵌入式调用复用。
//! 二进制入口仍在 `main.rs`，二者共享同一套模块，避免逻辑分叉。

pub mod config;
pub mod context;
pub mod dns;
pub mod geo;
pub mod health;
pub mod outbound;
pub mod profile;
pub mod relay;
pub mod reload;
pub mod router;
pub mod rules;
pub mod socks5;
pub mod udp;
