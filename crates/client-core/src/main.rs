//! `client-core`：最小 Rust 客户端数据面。
//!
//! 链路：`SOCKS5 入站 -> 规则分流 -> VLESS/WS/TLS/mlkem 出站`。
//! 复用 `v2node-proto` 的客户端加密握手。
//!
//! 已实现：TCP + UDP（SOCKS5 UDP ASSOCIATE / VLESS `command=2`）、
//! P0 规则引擎、GEOIP/GEOSITE、rule-set、fake-ip DNS、
//! `url-test`/`fallback` 健康检查、订阅热重载、文件日志、Windows 服务入口。
//!
//! 明确不做：TUN、完整 rule 生态、非 VLESS 协议。

use anyhow::Result;
use clap::Parser;

use client_core::{app, config::Config, logging, service};

fn main() -> Result<()> {
    let config = Config::parse();
    // 日志必须在任何业务动作前就绪：服务模式下 stdout 不可见，文件是唯一证据。
    let _log_guard = logging::init(config.log_dir.clone())?;

    if config.service {
        return service::run(config);
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        drop(tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            let _ = shutdown_tx.send(true);
        }));
        app::run(config, shutdown_rx).await
    })
}
