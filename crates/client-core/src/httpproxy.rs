//! 最小 HTTP 代理入站（混合端口）：CONNECT 隧道 + 绝对 URI 请求转发。
//!
//! 系统代理接管（P1.2）需要 WinINET 的 HTTP 代理协议；与 SOCKS5 共用
//! `--listen` 端口，按首字节分流（0x05 → SOCKS5，ASCII 方法 → HTTP）。
//! 路由/出站/计量与 SOCKS5 路径完全一致；不支持缓存与鉴权。

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::{
    context::AppContext,
    metrics, outbound, relay,
    router::Route,
    socks5::{Target, TargetAddr},
};

const MAX_HEAD: usize = 16 * 1024;

/// 处理一条 HTTP 代理连接。
///
/// # Errors
///
/// 请求格式非法、路由拒绝或出站失败时返回错误（尽力回 4xx/5xx）。
pub async fn serve(mut stream: TcpStream, context: Arc<AppContext>) -> Result<()> {
    let _active = metrics::on_accept(&context.metrics);
    stream.set_nodelay(true).context("set nodelay")?;
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let head_end = loop {
        let n = stream
            .read(&mut tmp)
            .await
            .context("read http proxy head")?;
        if n == 0 {
            bail!("http proxy client closed before head");
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
        if buf.len() > MAX_HEAD {
            bail!("http proxy head too large");
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.lines();
    let request_line = lines.next().context("missing request line")?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().context("missing method")?.to_owned();
    let target_uri = parts.next().context("missing target")?.to_owned();
    tracing::debug!(method = %method, target = %target_uri, "http proxy request");

    let (mut target, origin_form) = if method.eq_ignore_ascii_case("CONNECT") {
        (parse_authority(&target_uri)?, None)
    } else {
        let (authority, path) = split_absolute_uri(&target_uri)?;
        (parse_authority(&authority)?, Some(path))
    };

    let rule_target = crate::socks5::resolve_fake_ip(&mut target, &context.fakeip);
    let route = context.router().route(&rule_target)?;
    let connected = match route {
        Route::Proxy(proxy) => {
            tracing::debug!(dest = ?rule_target, proxy = %proxy.name, "http proxy route");
            outbound::connect_proxy(&proxy, &target).await
        }
        Route::Direct => {
            tracing::debug!(dest = ?rule_target, "http proxy direct");
            outbound::connect_direct(&target)
                .await
                .map(|stream| (stream, Vec::new()))
        }
        Route::Reject => {
            tracing::debug!(dest = ?rule_target, "http proxy reject");
            let _ = stream
                .write_all(
                    b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await;
            return Ok(());
        }
    };
    let (mut plain, pumps) = match connected {
        Ok(value) => value,
        Err(error) => {
            let _ = stream
                .write_all(
                    b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await;
            return Err(error);
        }
    };

    match origin_form {
        None => {
            stream
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .context("write connect response")?;
        }
        Some(path) => {
            let mut rewritten = format!("{method} {path} HTTP/1.1\r\n");
            for line in lines {
                let lower = line.to_ascii_lowercase();
                if lower.starts_with("proxy-connection:") || lower.starts_with("connection:") {
                    continue;
                }
                rewritten.push_str(line);
                rewritten.push_str("\r\n");
            }
            rewritten.push_str("Connection: close\r\n\r\n");
            plain
                .write_all(rewritten.as_bytes())
                .await
                .context("write rewritten head")?;
        }
    }
    stream.flush().await?;
    let result = relay::bidirectional(stream, plain, &context.metrics).await;
    for pump in pumps {
        pump.abort();
    }
    result
}

/// `host:port`（CONNECT）或绝对 URI 的 authority 部分。
fn parse_authority(authority: &str) -> Result<Target> {
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (
            host.to_owned(),
            port.parse::<u16>().context("invalid port")?,
        ),
        None => (authority.to_owned(), 80),
    };
    if host.is_empty() {
        bail!("empty host");
    }
    let addr = match host.parse::<std::net::IpAddr>() {
        Ok(ip) => TargetAddr::Ip(ip),
        Err(_) => TargetAddr::Domain(host),
    };
    Ok(Target { addr, port })
}

/// `http://host[:port]/path` → (authority, origin-form path)。
fn split_absolute_uri(uri: &str) -> Result<(String, String)> {
    let rest = uri
        .strip_prefix("http://")
        .context("only http:// absolute URIs supported")?;
    match rest.split_once('/') {
        Some((authority, path)) => Ok((authority.to_owned(), format!("/{path}"))),
        None => Ok((rest.to_owned(), "/".to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_authority, split_absolute_uri};
    use crate::socks5::TargetAddr;

    #[test]
    fn parses_authority_with_and_without_port() {
        let t = parse_authority("example.com:443").expect("parse");
        assert_eq!(t.port, 443);
        assert_eq!(t.addr, TargetAddr::Domain("example.com".to_owned()));
        let t = parse_authority("127.0.0.1:8080").expect("parse");
        assert_eq!(t.port, 8080);
        let t = parse_authority("example.com").expect("parse");
        assert_eq!(t.port, 80);
        assert!(parse_authority(":80").is_err());
    }

    #[test]
    fn splits_absolute_uri() {
        let (a, p) = split_absolute_uri("http://example.com:8080/x/y?z=1").expect("split");
        assert_eq!(a, "example.com:8080");
        assert_eq!(p, "/x/y?z=1");
        let (a, p) = split_absolute_uri("http://example.com").expect("split");
        assert_eq!(a, "example.com");
        assert_eq!(p, "/");
        assert!(split_absolute_uri("https://example.com/").is_err());
    }
}
