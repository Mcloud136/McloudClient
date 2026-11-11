//! 订阅拉取与配置热重载。
//!
//! 设计原则：
//! - **先构建、后替换**：新配置解析/校验失败时保留现有路由器，绝不因一次
//!   网络抖动或格式错误导致断网；
//! - **内容哈希去重**：订阅内容没变就不重建（大规则集重建约 95 ms）；
//! - **原子替换**：通过 [`AppContext`] 的 `ArcSwap` 无锁切换，读路径无锁；
//! - **支持本地文件与 HTTP(S)**：Rust 侧不引入 YAML 依赖，订阅需为
//!   `script/clash2profile.py` 产出的 JSON profile。

use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use rustls::{ClientConfig, RootCertStore, pki_types::ServerName};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    time::interval,
};
use tokio_rustls::TlsConnector;

use crate::{
    context::AppContext, geo::GeoData, outbound::AsyncReadWrite, profile::Profile, router::Router,
};

/// 配置来源。
#[derive(Debug, Clone)]
pub enum Source {
    /// 本地 JSON profile 文件。
    File(PathBuf),
    /// 远程订阅地址（返回 JSON profile）。
    Url(String),
}

/// 定时重载规格。
#[derive(Debug, Clone)]
pub struct ReloadSpec {
    pub source: Source,
    pub interval: Duration,
}

/// 计算内容哈希，用于判断订阅是否发生变化。
#[must_use]
pub fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

/// 读取一次配置内容。
///
/// # Errors
///
/// 本地文件读取失败、URL 非法、网络/TLS 失败或 HTTP 非 200 时返回错误。
pub async fn fetch(source: &Source) -> Result<Vec<u8>> {
    match source {
        Source::File(path) => {
            std::fs::read(path).with_context(|| format!("read profile {}", path.display()))
        }
        Source::Url(url) => fetch_url(url).await,
    }
}

/// 由配置字节构建路由器；解析或校验失败时返回错误，不影响现有配置。
///
/// # Errors
///
/// JSON 解析失败或 profile 校验失败时返回错误。
pub fn build_router(bytes: &[u8], geo: Arc<GeoData>) -> Result<Router> {
    let profile: Profile = serde_json::from_slice(bytes)
        .context("parse profile JSON (YAML needs clash2profile.py)")?;
    Router::from_profile(profile, geo)
}

/// 启动定时重载任务。
pub fn spawn(context: Arc<AppContext>, spec: ReloadSpec, geo: Arc<GeoData>, initial_hash: u64) {
    tracing::info!(
        interval_secs = spec.interval.as_secs(),
        source = ?spec.source,
        "profile auto-reload enabled"
    );
    drop(tokio::spawn(async move {
        let mut ticker = interval(spec.interval);
        ticker.tick().await; // 跳过立即触发的第一次
        let mut last_hash = initial_hash;
        loop {
            ticker.tick().await;
            match reload_once(&context, &spec, &geo, last_hash).await {
                Ok(Some(hash)) => last_hash = hash,
                Ok(None) => {}
                Err(error) => {
                    // 关键：失败时保留当前配置，不打断在线连接。
                    tracing::warn!(%error, "profile reload failed, keeping current config");
                }
            }
        }
    }));
}

async fn reload_once(
    context: &Arc<AppContext>,
    spec: &ReloadSpec,
    geo: &Arc<GeoData>,
    last_hash: u64,
) -> Result<Option<u64>> {
    let bytes = fetch(&spec.source).await?;
    let hash = hash_bytes(&bytes);
    if hash == last_hash {
        return Ok(None);
    }
    // 先完整构建（含 rule-set 读取与校验），成功后再替换。
    let router = build_router(&bytes, Arc::clone(geo))?;
    context.replace_router(router);
    Ok(Some(hash))
}

/// 最小 HTTP/1.1 客户端（支持 `http://` 与 `https://`）。
async fn fetch_url(url: &str) -> Result<Vec<u8>> {
    let (scheme, rest) = url
        .split_once("://")
        .context("profile url must include http:// or https://")?;
    let (authority, path) = match rest.split_once('/') {
        Some((authority, tail)) => (authority, format!("/{tail}")),
        None => (rest, "/".to_owned()),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (
            host.to_owned(),
            port.parse::<u16>().context("invalid url port")?,
        ),
        None => (
            authority.to_owned(),
            if scheme == "https" { 443 } else { 80 },
        ),
    };
    let stream = TcpStream::connect((host.as_str(), port))
        .await
        .with_context(|| format!("connect {host}:{port}"))?;

    let mut connection: Box<dyn AsyncReadWrite> = match scheme {
        "http" => Box::new(stream),
        "https" => {
            let connector = tls_connector()?;
            let server_name = ServerName::try_from(host.clone())
                .map_err(|_| anyhow::anyhow!("invalid TLS server name {host}"))?;
            Box::new(
                connector
                    .connect(server_name, stream)
                    .await
                    .context("TLS connect")?,
            )
        }
        other => bail!("unsupported profile url scheme {other}"),
    };

    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {authority}\r\nUser-Agent: client-core/0.1\r\n\
         Accept: */*\r\nAccept-Encoding: identity\r\nConnection: close\r\n\r\n"
    );
    connection
        .write_all(request.as_bytes())
        .await
        .context("write profile request")?;
    let mut response = Vec::new();
    connection
        .read_to_end(&mut response)
        .await
        .context("read profile response")?;
    parse_response(&response)
}

/// 解析 HTTP 响应体（支持 Content-Length 与 chunked）。
fn parse_response(raw: &[u8]) -> Result<Vec<u8>> {
    let split = find_subslice(raw, b"\r\n\r\n").context("malformed HTTP response")?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let mut lines = head.lines();
    let status = lines.next().context("missing HTTP status line")?;
    if !status.contains(" 200") {
        bail!("profile server returned {status}");
    }
    let mut chunked = false;
    let mut length: Option<usize> = None;
    for line in lines {
        if let Some((key, value)) = line.split_once(':') {
            match key.trim().to_ascii_lowercase().as_str() {
                "transfer-encoding" if value.contains("chunked") => chunked = true,
                "content-length" => length = value.trim().parse().ok(),
                _ => {}
            }
        }
    }
    let body = &raw[split + 4..];
    if chunked {
        return decode_chunked(body);
    }
    Ok(match length {
        Some(size) => body[..size.min(body.len())].to_vec(),
        None => body.to_vec(),
    })
}

fn decode_chunked(mut data: &[u8]) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    loop {
        let newline = find_subslice(data, b"\r\n").context("malformed chunk header")?;
        let header = std::str::from_utf8(&data[..newline]).context("chunk header not utf-8")?;
        let size_text = header.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16).context("invalid chunk size")?;
        data = &data[newline + 2..];
        if size == 0 {
            break;
        }
        if data.len() < size + 2 {
            bail!("truncated chunked body");
        }
        output.extend_from_slice(&data[..size]);
        data = &data[size + 2..];
    }
    Ok(output)
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn tls_connector() -> Result<TlsConnector> {
    let mut roots = RootCertStore::empty();
    for certificate in rustls_native_certs::load_native_certs().certs {
        let _ = roots.add(certificate);
    }
    if roots.is_empty() {
        bail!("no native CA certificates available");
    }
    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(TlsConnector::from(Arc::new(config)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_content_length_response() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhelloTAIL";
        assert_eq!(parse_response(raw).unwrap(), b"hello");
    }

    #[test]
    fn parses_chunked_response() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n";
        assert_eq!(parse_response(raw).unwrap(), b"hello");
    }

    #[test]
    fn rejects_non_200() {
        let raw = b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n";
        assert!(parse_response(raw).is_err());
    }

    #[test]
    fn hash_changes_with_content() {
        assert_ne!(hash_bytes(b"a"), hash_bytes(b"b"));
        assert_eq!(hash_bytes(b"a"), hash_bytes(b"a"));
    }

    #[test]
    fn rejects_yaml_as_profile() {
        let err = build_router(b"proxies: []\nrules: []\n", Arc::new(GeoData::default()));
        assert!(err.is_err());
    }
}
