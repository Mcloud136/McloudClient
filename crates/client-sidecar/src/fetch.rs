//! 最小 HTTP/1.1 客户端，供订阅拉取共用。
//!
//! 不引入完整 HTTP 客户端：只需要 `GET` + `200` + `Content-Length` / chunked。
//! 订阅 URL 含 `token`，因此本模块不打印 URL、不记录响应体。

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use rustls::{ClientConfig, RootCertStore, pki_types::ServerName};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
};
use tokio_rustls::TlsConnector;

/// 统一读写对象，避免为 TLS 与明文各写一份。
trait AsyncReadWrite: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncReadWrite for T {}

/// 拉取一次订阅内容（支持 `http://` 与 `https://`）。
///
/// # Errors
///
/// URL 非法、连接/TLS 失败、HTTP 非 200 或响应格式错误时返回错误。
pub async fn fetch_url(url: &str) -> Result<Vec<u8>> {
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
        "GET {path} HTTP/1.1\r\nHost: {authority}\r\nUser-Agent: client-sidecar/0.1\r\n\
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

/// 解析 HTTP 响应体（支持 `Content-Length` 与 chunked）。
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
    use super::parse_response;

    #[test]
    fn parses_content_length_response() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhelloTAIL";
        assert_eq!(parse_response(raw).expect("parse"), b"hello");
    }

    #[test]
    fn parses_chunked_response() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n";
        assert_eq!(parse_response(raw).expect("parse"), b"hello");
    }

    #[test]
    fn rejects_non_200() {
        let raw = b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n";
        assert!(parse_response(raw).is_err());
    }
}
