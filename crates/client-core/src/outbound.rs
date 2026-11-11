//! 出站：VLESS/WS/TLS/mlkem 代理出站，以及 DIRECT 直连出站。

use std::{net::IpAddr, sync::Arc};

use anyhow::{Context, Result, bail};
use futures::{SinkExt, StreamExt};
use rustls::{
    ClientConfig, DigitallySignedStruct, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, ServerName, UnixTime},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream},
    net::TcpStream,
    task::JoinHandle,
};
use tokio_tungstenite::{
    Connector, WebSocketStream, connect_async_tls_with_config,
    tungstenite::{Message, client::IntoClientRequest, http::HeaderValue},
};
use v2node_proto::encryption::{self, ClientEncryptionConfig};

use crate::{
    profile::Proxy,
    socks5::{Target, TargetAddr},
};

/// 统一出站流类型（代理出站是内存双工流，DIRECT 出站是 TCP 流）。
pub trait AsyncReadWrite: AsyncRead + AsyncWrite + Unpin + Send {}

impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncReadWrite for T {}

/// VLESS 命令字：TCP 代理。
const VLESS_CMD_TCP: u8 = 1;
/// VLESS 命令字：UDP 中继（一条连接绑定一个目标）。
const VLESS_CMD_UDP: u8 = 2;

/// 建立到节点的 VLESS TCP 连接，返回明文流与需要保活的 pump 任务。
///
/// # Errors
///
/// WebSocket/TLS 连接失败、加密握手失败或 VLESS 响应头异常时返回错误。
pub async fn connect_proxy(
    proxy: &Proxy,
    target: &Target,
) -> Result<(Box<dyn AsyncReadWrite>, Vec<JoinHandle<()>>)> {
    connect_vless(proxy, target, VLESS_CMD_TCP).await
}

/// 建立到节点的 VLESS UDP 连接（`command=2`）。
///
/// 服务端会把这条连接上收到的每个 `length + payload` 数据报发往
/// 请求头里指定的同一个目标，因此调用方需要按目标分别建连。
///
/// # Errors
///
/// WebSocket/TLS 连接失败、加密握手失败或 VLESS 响应头异常时返回错误。
pub async fn connect_proxy_udp(
    proxy: &Proxy,
    target: &Target,
) -> Result<(Box<dyn AsyncReadWrite>, Vec<JoinHandle<()>>)> {
    connect_vless(proxy, target, VLESS_CMD_UDP).await
}

async fn connect_vless(
    proxy: &Proxy,
    target: &Target,
    command: u8,
) -> Result<(Box<dyn AsyncReadWrite>, Vec<JoinHandle<()>>)> {
    let url = websocket_url(proxy);
    let mut request = url.clone().into_client_request()?;
    request
        .headers_mut()
        .insert("Host", HeaderValue::from_str(&proxy.ws_host())?);
    request
        .headers_mut()
        .insert("User-Agent", HeaderValue::from_static("client-core/0.1"));

    let (ws, _) = if proxy.skip_cert_verify && url.starts_with("wss://") {
        let tls = ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerify))
            .with_no_client_auth();
        connect_async_tls_with_config(request, None, true, Some(Connector::Rustls(Arc::new(tls))))
            .await
            .context("websocket TLS connect")?
    } else {
        connect_async_tls_with_config(request, None, true, None)
            .await
            .context("websocket connect")?
    };

    let (app, ws_pump) = ws_to_io(ws, 65536);
    let mut pumps = vec![ws_pump];
    let mut plain = if let Some(encryption) = &proxy.encryption {
        let enc_config =
            ClientEncryptionConfig::parse(encryption).context("parse encryption config")?;
        let (plain, enc_pump) = encryption::client_handshake(app, &enc_config)
            .await
            .context("client encryption handshake")?;
        pumps.push(enc_pump);
        plain
    } else {
        app
    };

    let header = vless_header(&proxy.uuid, target, command)?;
    plain
        .write_all(&header)
        .await
        .context("write VLESS request header")?;
    plain.flush().await?;

    let mut response = [0u8; 2];
    plain
        .read_exact(&mut response)
        .await
        .context("read VLESS response header")?;
    if response != [0u8, 0u8] {
        bail!("bad VLESS response header {response:?}");
    }
    Ok((Box::new(plain), pumps))
}

/// 直连目标（`DIRECT` 策略）。
///
/// # Errors
///
/// 目标连接失败时返回错误。
pub async fn connect_direct(target: &Target) -> Result<Box<dyn AsyncReadWrite>> {
    let stream = match &target.addr {
        TargetAddr::Domain(domain) => TcpStream::connect((domain.as_str(), target.port)).await,
        TargetAddr::Ip(ip) => TcpStream::connect((*ip, target.port)).await,
    }
    .context("direct connect")?;
    stream.set_nodelay(true).context("set nodelay")?;
    Ok(Box::new(stream))
}

fn websocket_url(proxy: &Proxy) -> String {
    let scheme = if proxy.tls { "wss" } else { "ws" };
    let path = proxy.ws_path();
    let path = if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("/{path}")
    };
    format!("{scheme}://{}:{}{path}", proxy.server, proxy.port)
}

fn vless_header(uuid: &str, target: &Target, command: u8) -> Result<Vec<u8>> {
    let uuid = uuid::Uuid::parse_str(uuid).context("parse uuid")?;
    let mut data = Vec::with_capacity(1 + 16 + 1 + 1 + 2 + 1 + 16);
    data.push(0); // version
    data.extend_from_slice(uuid.as_bytes());
    data.push(0); // addon length
    data.push(command);
    data.extend_from_slice(&target.port.to_be_bytes());
    match &target.addr {
        TargetAddr::Ip(IpAddr::V4(ip)) => {
            data.push(1);
            data.extend_from_slice(&ip.octets());
        }
        TargetAddr::Domain(domain) => {
            let len = u8::try_from(domain.len()).context("target domain too long")?;
            data.push(2);
            data.push(len);
            data.extend_from_slice(domain.as_bytes());
        }
        TargetAddr::Ip(IpAddr::V6(ip)) => {
            data.push(3);
            data.extend_from_slice(&ip.octets());
        }
    }
    Ok(data)
}

fn ws_to_io<S>(mut ws: WebSocketStream<S>, capacity: usize) -> (DuplexStream, JoinHandle<()>)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (app, pump) = tokio::io::duplex(capacity);
    let (mut pump_reader, mut pump_writer) = tokio::io::split(pump);
    let join = tokio::spawn(async move {
        let mut buffer = vec![0u8; capacity.max(1024)];
        loop {
            tokio::select! {
                message = ws.next() => {
                    match message {
                        Some(Ok(Message::Binary(data))) => {
                            if pump_writer.write_all(&data).await.is_err() { break; }
                        }
                        Some(Ok(Message::Text(text))) => {
                            if pump_writer.write_all(text.as_str().as_bytes()).await.is_err() { break; }
                        }
                        Some(Ok(Message::Ping(data))) => {
                            let _ = ws.send(Message::Pong(data)).await;
                        }
                        Some(Ok(Message::Close(_))) | None => break,
                        Some(Ok(_)) => {}
                        Some(Err(_)) => break,
                    }
                }
                read = pump_reader.read(&mut buffer) => {
                    match read {
                        Ok(0) => {
                            let _ = ws.send(Message::Close(None)).await;
                            break;
                        }
                        Ok(read) => {
                            if ws.send(Message::Binary(buffer[..read].to_vec().into())).await.is_err() { break; }
                        }
                        Err(_) => break,
                    }
                }
            }
        }
        let _ = pump_writer.shutdown().await;
    });
    (app, join)
}

/// 仅测试用：跳过证书校验。
#[derive(Debug)]
struct NoVerify;

impl ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::aws_lc_rs::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}
