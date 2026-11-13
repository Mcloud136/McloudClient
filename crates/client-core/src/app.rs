//! 内核启动与主循环，供 CLI 与服务两种入口复用。

use std::{path::PathBuf, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use tokio::{net::TcpListener, sync::watch};

use crate::{
    config::Config,
    context::AppContext,
    dns::{self, DnsServer, FakeIp},
    geo::GeoData,
    httpproxy, ipc,
    reload::{self, ReloadSpec, Source},
    socks5,
};

/// 启动全部组件并进入 accept 循环，直到 `shutdown` 置位或发送端被丢弃。
///
/// # Errors
///
/// geo 构建、初始配置加载、router 构建、端口绑定等任一失败都会返回错误；
/// 初始配置失败时不会以空配置启动。
pub async fn run(config: Config, mut shutdown: watch::Receiver<bool>) -> Result<()> {
    let geo = Arc::new(config.build_geo().context("build geo data")?);
    log_geo(&geo);

    // 初次加载：失败直接退出，避免以空配置提供服务。
    let (initial_bytes, source) = load_initial(&config).await?;
    let initial_hash = reload::hash_bytes(&initial_bytes);
    let router = reload::build_router(&initial_bytes, Arc::clone(&geo)).context("build router")?;
    tracing::info!(rules = router.rule_count(), "router ready");

    let context = AppContext::new(router, Arc::new(FakeIp::default()));

    // 控制面接口：仅在显式指定管道名时启用，缺省不改变原有行为。
    match (&config.ipc_pipe, source.clone()) {
        (Some(pipe), Some(source)) => {
            ipc::spawn(
                pipe.clone(),
                Arc::clone(&context),
                source,
                Arc::clone(&geo),
                ipc::KernelEnv {
                    log_path: Some(crate::logging::current_path(
                        &config
                            .log_dir
                            .clone()
                            .unwrap_or_else(crate::logging::default_dir),
                        "core",
                    )),
                    dns_enabled: config.dns_listen.is_some(),
                },
            );
        }
        (Some(_), None) => tracing::warn!("--ipc-pipe ignored in single-node mode"),
        (None, _) => {}
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
        tokio::select! {
            () = wait_shutdown(&mut shutdown) => {
                tracing::info!("shutdown requested, stopping data plane");
                break;
            }
            accepted = listener.accept() => {
                let (stream, peer) = accepted.context("accept inbound")?;
                let session_context = Arc::clone(&context);
                drop(tokio::spawn(async move {
                    // 混合端口：0x05 → SOCKS5；ASCII 方法 → HTTP 代理（系统代理接管）。
                    let mut first = [0u8; 1];
                    let is_socks = match stream.peek(&mut first).await {
                        Ok(_) => first[0] == socks5::SOCKS5_VERSION,
                        Err(error) => {
                            tracing::debug!(%peer, %error, "inbound peek failed");
                            return;
                        }
                    };
                    let result = if is_socks {
                        socks5::serve(stream, session_context).await
                    } else {
                        httpproxy::serve(stream, session_context).await
                    };
                    if let Err(error) = result {
                        if is_socks {
                            tracing::debug!(%peer, %error, "inbound session closed");
                        } else {
                            tracing::info!(%peer, %error, "http proxy session error");
                        }
                    }
                }));
            }
        }
    }

    Ok(())
}

/// 等待关闭信号；发送端被丢弃也视为关闭，避免进程无法退出。
async fn wait_shutdown(shutdown: &mut watch::Receiver<bool>) {
    if *shutdown.borrow() {
        return;
    }
    let _ = shutdown.changed().await;
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
