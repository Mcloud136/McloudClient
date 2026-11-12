//! 最小 HTTP/1.1 客户端，供订阅拉取与面板 API 共用。
//!
//! 不引入完整 HTTP 客户端：`GET`/`POST` + 自定义头 + JSON 体 +
//! `Content-Length` / chunked 响应。订阅 URL 与面板凭据敏感，
//! 本模块不打印 URL、请求体、响应体与 Authorization 头。

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

/// HTTP 响应：状态码 + 原始 body。业务层自行解释 JSON 与错误码。
#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

/// 拉取一次订阅内容（支持 `http://` 与 `https://`），要求 200。
///
/// # Errors
///
/// URL 非法、连接/TLS 失败、HTTP 非 200 或响应格式错误时返回错误。
pub async fn fetch_url(url: &str) -> Result<Vec<u8>> {
    let response = request("GET", url, &[], None).await?;
    if response.status != 200 {
        bail!("profile server returned HTTP {}", response.status);
    }
    Ok(response.body)
}

/// 发送一次 HTTP 请求（`http://` 与 `https://`），返回状态码与 body。
///
/// `headers` 为附加请求头（如 `Authorization`、`Content-Type`）；
/// `body` 为请求体（POST JSON 等）。头名/头值含 CR/LF/NUL 时拒绝，
/// 防止经由用户可配置站点地址注入额外请求头。
///
/// # Errors
///
/// URL 非法、头非法、连接/TLS/读写失败或响应格式错误时返回错误。
pub async fn request(
    method: &str,
    url: &str,
    headers: &[(String, String)],
    body: Option<&[u8]>,
) -> Result<HttpResponse> {
    let (scheme, rest) = url
        .split_once("://")
        .context("url must include http:// or https://")?;
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
    for (name, value) in headers {
        validate_header(name, value)?;
    }
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
        other => bail!("unsupported url scheme {other}"),
    };

    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {authority}\r\nUser-Agent: client-sidecar/0.2\r\n\
         Accept: */*\r\nAccept-Encoding: identity\r\nConnection: close\r\n"
    );
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    if let Some(body) = body {
        request.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    request.push_str("\r\n");
    connection
        .write_all(request.as_bytes())
        .await
        .context("write request head")?;
    if let Some(body) = body {
        connection
            .write_all(body)
            .await
            .context("write request body")?;
    }
    let mut response = Vec::new();
    connection
        .read_to_end(&mut response)
        .await
        .context("read response")?;
    parse_response(&response)
}

/// 头名/头值不得含 CR/LF/NUL；头名不得含空格或冒号。
fn validate_header(name: &str, value: &str) -> Result<()> {
    let bad = |c: char| c == '\r' || c == '\n' || c == '\0';
    if name.is_empty() || name.chars().any(|c| bad(c) || c == ' ' || c == ':') {
        bail!("invalid header name");
    }
    if value.chars().any(bad) {
        bail!("invalid header value");
    }
    Ok(())
}

/// 解析 HTTP 响应：状态码 + body（支持 `Content-Length` 与 chunked）。
fn parse_response(raw: &[u8]) -> Result<HttpResponse> {
    let split = find_subslice(raw, b"\r\n\r\n").context("malformed HTTP response")?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let mut lines = head.lines();
    let status_line = lines.next().context("missing HTTP status line")?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .context("missing HTTP status code")?
        .parse::<u16>()
        .context("invalid HTTP status code")?;
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
    let body = if chunked {
        decode_chunked(body)?
    } else {
        match length {
            Some(size) => body[..size.min(body.len())].to_vec(),
            None => body.to_vec(),
        }
    };
    Ok(HttpResponse { status, body })
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
    use super::{parse_response, request, validate_header};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn parses_content_length_response() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhelloTAIL";
        let response = parse_response(raw).expect("parse");
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"hello");
    }

    #[test]
    fn parses_chunked_response() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n";
        let response = parse_response(raw).expect("parse");
        assert_eq!(response.body, b"hello");
    }

    #[test]
    fn keeps_non_200_status_and_body() {
        let raw =
            "HTTP/1.1 429 Too Many Requests\r\nContent-Length: 17\r\n\r\n{\"message\":\"慢\"}"
                .as_bytes();
        let response = parse_response(raw).expect("parse");
        assert_eq!(response.status, 429);
        assert_eq!(response.body, "{\"message\":\"慢\"}".as_bytes());
    }

    #[test]
    fn rejects_header_injection() {
        assert!(validate_header("X-Ok", "value").is_ok());
        assert!(validate_header("X-Bad", "a\r\nInjected: 1").is_err());
        assert!(validate_header("X-Bad\n", "v").is_err());
        assert!(validate_header("", "v").is_err());
        assert!(validate_header("X:Bad", "v").is_err());
    }

    #[tokio::test]
    async fn post_json_roundtrip_with_headers() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut buf = Vec::new();
            let mut tmp = [0u8; 4096];
            loop {
                let n = socket.read(&mut tmp).await.expect("read");
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
                if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&buf[..pos]).to_string();
                    let length: usize = head
                        .lines()
                        .find_map(|l| l.strip_prefix("Content-Length: "))
                        .and_then(|v| v.trim().parse().ok())
                        .unwrap_or(0);
                    while buf.len() < pos + 4 + length {
                        let n = socket.read(&mut tmp).await.expect("read body");
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                    }
                    assert!(head.starts_with("POST /api/v1/passport/auth/login "));
                    assert!(head.contains("Authorization: mock-jwt"));
                    assert!(head.contains("Content-Type: application/json"));
                    let body = &buf[pos + 4..pos + 4 + length];
                    assert_eq!(body, br#"{"email":"a@b.c","password":"12345678"}"#);
                    break;
                }
            }
            let response = b"HTTP/1.1 200 OK\r\nContent-Length: 21\r\n\r\n{\"data\":{\"ok\":true}}";
            socket.write_all(response).await.expect("write");
        });
        let response = request(
            "POST",
            &format!("http://{addr}/api/v1/passport/auth/login"),
            &[
                ("Authorization".to_owned(), "mock-jwt".to_owned()),
                ("Content-Type".to_owned(), "application/json".to_owned()),
            ],
            Some(br#"{"email":"a@b.c","password":"12345678"}"#),
        )
        .await
        .expect("request");
        server.await.expect("server");
        assert_eq!(response.status, 200);
        assert_eq!(response.body, br#"{"data":{"ok":true}}"#);
    }
}
