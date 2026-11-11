//! 策略组健康检查。
//!
//! 对 `url-test` / `fallback` 组的每个成员，经该节点发起一次明文 HTTP
//! 请求并计时，把结果写入 [`Router`] 的健康表，供路由时选择。
//!
//! 只支持 `http://`：`https://` 需要额外的 TLS 栈与证书校验，而标准
//! 探测地址（`generate_204`）本身就是明文的。

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    task::JoinHandle,
    time::{interval, timeout},
};

use crate::{
    context::AppContext,
    outbound,
    profile::{HealthSpec, Proxy},
    socks5::{Target, TargetAddr},
};

/// 单次探测超时。
///
/// 取 10 秒而非 Clash 默认的 5 秒：跨境链路叠加 WS/TLS/mlkem 握手后，
/// 国内构建机实测单次探测可达 5 秒以上，5 秒阈值会产生假失败。
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
/// 未配置 `interval` 时的默认间隔。
const DEFAULT_INTERVAL_SECS: u64 = 300;

/// 探测目标。
#[derive(Debug, Clone)]
struct Endpoint {
    host: String,
    port: u16,
    path: String,
}

impl Endpoint {
    fn parse(url: &str) -> Option<Self> {
        let rest = url.strip_prefix("http://")?;
        let (authority, path) = match rest.split_once('/') {
            Some((authority, tail)) => (authority, format!("/{tail}")),
            None => (rest, String::from("/")),
        };
        if authority.is_empty() {
            return None;
        }
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) => (host.to_owned(), port.parse().ok()?),
            None => (authority.to_owned(), 80),
        };
        if host.is_empty() {
            return None;
        }
        Some(Self { host, port, path })
    }
}

/// 为所有 `url-test` / `fallback` 组启动健康检查任务。
#[must_use]
pub fn spawn_all(context: Arc<AppContext>, specs: Vec<HealthSpec>) -> Vec<JoinHandle<()>> {
    specs
        .into_iter()
        .map(|spec| tokio::spawn(run_group(Arc::clone(&context), spec)))
        .collect()
}

async fn run_group(context: Arc<AppContext>, spec: HealthSpec) {
    let Some(endpoint) = Endpoint::parse(&spec.url) else {
        tracing::warn!(
            group = %spec.group,
            url = %spec.url,
            "health check skipped: only http:// urls are supported"
        );
        return;
    };
    let secs = if spec.interval.is_zero() {
        DEFAULT_INTERVAL_SECS
    } else {
        spec.interval.as_secs()
    };
    tracing::info!(
        group = %spec.group,
        kind = %spec.kind,
        members = spec.members.len(),
        interval_secs = secs,
        tolerance_ms = spec.tolerance,
        url = %spec.url,
        "health check started"
    );
    let mut ticker = interval(Duration::from_secs(secs));
    loop {
        ticker.tick().await;
        for proxy in &spec.members {
            let latency = probe(proxy, &endpoint).await;
            // 每次取当前路由器：配置重载后旧任务会被中止，这里只是兜底。
            context.router().set_health(&proxy.name, latency);
            match latency {
                Some(ms) => {
                    tracing::debug!(group = %spec.group, node = %proxy.name, ms, "health ok");
                }
                None => {
                    tracing::debug!(group = %spec.group, node = %proxy.name, "health failed");
                }
            }
        }
    }
}

/// 经指定节点请求探测地址，返回端到端耗时（毫秒）。
async fn probe(proxy: &Proxy, endpoint: &Endpoint) -> Option<u32> {
    let target = Target {
        addr: TargetAddr::Domain(endpoint.host.clone()),
        port: endpoint.port,
    };
    let started = Instant::now();
    let Ok(Ok((mut stream, pumps))) =
        timeout(PROBE_TIMEOUT, outbound::connect_proxy(proxy, &target)).await
    else {
        return None;
    };
    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: client-core-health\r\nConnection: close\r\n\r\n",
        endpoint.path, endpoint.host
    );
    let responded = async {
        stream.write_all(request.as_bytes()).await.ok()?;
        stream.flush().await.ok()?;
        let mut buffer = [0u8; 32];
        let read = timeout(PROBE_TIMEOUT, stream.read(&mut buffer))
            .await
            .ok()?
            .ok()?;
        (read > 0).then_some(())
    }
    .await;
    for pump in pumps {
        pump.abort();
    }
    responded?;
    Some(u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_http_url() {
        let endpoint = Endpoint::parse("http://www.gstatic.com/generate_204").unwrap();
        assert_eq!(endpoint.host, "www.gstatic.com");
        assert_eq!(endpoint.port, 80);
        assert_eq!(endpoint.path, "/generate_204");
    }

    #[test]
    fn parses_custom_port_and_root() {
        let endpoint = Endpoint::parse("http://example.com:8080").unwrap();
        assert_eq!(endpoint.host, "example.com");
        assert_eq!(endpoint.port, 8080);
        assert_eq!(endpoint.path, "/");
    }

    #[test]
    fn rejects_https_and_garbage() {
        assert!(Endpoint::parse("https://example.com/x").is_none());
        assert!(Endpoint::parse("example.com").is_none());
        assert!(Endpoint::parse("http://").is_none());
    }
}
