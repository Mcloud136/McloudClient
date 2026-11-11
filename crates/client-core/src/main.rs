//! `client-core`：最小 Rust 客户端数据面。
//!
//! 链路：`SOCKS5 入站 -> 规则分流 -> VLESS/WS/TLS/mlkem 出站`。
//! 复用 `v2node-proto` 的客户端加密握手。
//!
//! 已实现：TCP + UDP（SOCKS5 UDP ASSOCIATE / VLESS `command=2`）、
//! P0 规则引擎、GEOIP/GEOSITE、rule-set、fake-ip DNS、
//! `url-test`/`fallback` 健康检查、订阅热重载。
//!
//! 明确不做：TUN、完整 rule 生态、非 VLESS 协议。

use std::{path::PathBuf, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use clap::Parser;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

use client_core::{
    config::Config,
    context::AppContext,
    dns::{self, DnsServer, FakeIp},
    geo::GeoData,
    reload::{self, ReloadSpec, Source},
    socks5,
};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();
    let config = Config::parse();

    let geo = Arc::new(config.build_geo().context("build geo data")?);
    log_geo(&geo);

    // 初次加载：失败直接退出，避免以空配置提供服务。
    let (initial_bytes, source) = load_initial(&config).await?;
    let initial_hash = reload::hash_bytes(&initial_bytes);
    let router = reload::build_router(&initial_bytes, Arc::clone(&geo)).context("build router")?;
    tracing::info!(rules = router.rule_count(), "router ready");

    let context = AppContext::new(router, Arc::new(FakeIp::default()));

    // 控制面接口：仅在显式指定管道名时启用，缺省不改变原有行为。
    if let Some(pipe) = &config.ipc_pipe {
        if let Some(source) = source.clone() {
            client_core::ipc::spawn(pipe.clone(), Arc::clone(&context), source, Arc::clone(&geo));
        } else {
            tracing::warn!("--ipc-pipe ignored in single-node mode");
        }
    }

    if config.reload_interval > 0 {
        if let Some(source) = source {
            let spec = ReloadSpec {
                source,
                interval: Duration::from_secs(config.reload_interval),
            };
            reload::spawn(Arc::clone(&context), spec, Arc::clone(&geo), initial_hash);
        } else {
            tracing::warn!("--reload-interval ignored in single-node mode");
        }
    }

    if let Some(listen) = &config.dns_listen {
        let upstream = config.dns_upstream_addr().context("dns upstream")?;
        let socket = Arc::new(dns::bind(listen).await?);
        let server = Arc::new(DnsServer::new(Arc::clone(&context), upstream));
        tracing::info!(listen = %listen, %upstream, "fake-ip dns listening");
        drop(tokio::spawn(server.serve(socket)));
        spawn_fakeip_report(Arc::clone(&context.fakeip));
    }

    let listener = TcpListener::bind(&config.listen)
        .await
        .with_context(|| format!("bind {}", config.listen))?;
    tracing::info!(
        listen = %config.listen,
        from_profile = config.profile.is_some(),
        fakeip = config.dns_listen.is_some(),
        "client-core listening"
    );
    loop {
        let (stream, peer) = listener.accept().await.context("accept socks5")?;
        let session_context = Arc::clone(&context);
        drop(tokio::spawn(async move {
            if let Err(error) = socks5::serve(stream, session_context).await {
                tracing::debug!(%peer, %error, "socks5 session closed");
            }
        }));
    }
}

/// 读取初始配置，返回字节与（可重载时的）来源。
async fn load_initial(config: &Config) -> Result<(Vec<u8>, Option<Source>)> {
    if let Some(url) = &config.profile_url {
        let source = Source::Url(url.clone());
        let bytes = reload::fetch(&source)
            .await
            .context("fetch initial profile")?;
        return Ok((bytes, Some(source)));
    }
    if let Some(path) = &config.profile {
        let source = Source::File(PathBuf::from(path));
        let bytes = reload::fetch(&source)
            .await
            .context("read initial profile")?;
        return Ok((bytes, Some(source)));
    }
    // 单节点模式：合成 profile 后走同一条构建路径。
    let profile = config.build_profile().context("build profile")?;
    let bytes = serde_json::to_vec(&profile).context("serialize synthetic profile")?;
    Ok((bytes, None))
}

fn log_geo(geo: &GeoData) {
    if let Some(geoip) = &geo.geoip_cn {
        if geoip.is_empty() {
            tracing::warn!("geoip CN data is empty");
        } else {
            tracing::info!(cidrs = geoip.len(), "geoip CN loaded");
        }
    }
    if let Some(geosite) = &geo.geosite_cn {
        if geosite.is_empty() {
            tracing::warn!("geosite CN data is empty");
        } else {
            tracing::info!(rules = geosite.len(), "geosite CN loaded");
        }
    }
}

/// 每 5 分钟上报一次 fake-ip 池规模，便于观察映射是否异常增长。
fn spawn_fakeip_report(fakeip: Arc<FakeIp>) {
    drop(tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(300));
        ticker.tick().await; // 跳过立即触发的第一次
        loop {
            ticker.tick().await;
            if fakeip.is_empty() {
                tracing::debug!("fake-ip pool empty");
            } else {
                tracing::info!(entries = fakeip.len(), "fake-ip pool");
            }
        }
    }));
}
