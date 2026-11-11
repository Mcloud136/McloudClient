//! SOCKS5 UDP ASSOCIATE 与 VLESS UDP（`command=2`）中继。
//!
//! 协议事实（对齐服务端 `src/vless.rs` 的 `relay_udp`）：
//!
//! - 目标地址只在 VLESS 请求头里出现一次，服务端据此 `connect` 一个
//!   `UdpSocket`，因此**一条 VLESS 连接只服务一个目标**；
//! - 连接上的载荷是 `length(u16 BE) + payload`，上下行同构。
//!
//! 因此这里按 `(目标, 出站策略)` 维护会话：首个数据报触发建连（握手在
//! 后台任务完成，期间数据报进队列），之后复用同一条连接；会话空闲
//! 超时后自动释放。
//!
//! 实现细节：VLESS 的 `length` 头是**有状态**的（读满 2 字节才能解析），
//! 若把读操作放进 `select!` 里被取消会丢失半个头、导致后续帧全部错位。
//! 所以下行读取放在独立任务中独占 reader，上行写独占 writer，主循环只
//! 负责在两者之间搬运数据并判空闲。

use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpStream, UdpSocket},
    sync::{Mutex, mpsc},
    time::timeout,
};

use crate::{
    context::AppContext,
    outbound,
    profile::Proxy,
    router::Route,
    socks5::{
        ATYP_DOMAIN, ATYP_IPV4, ATYP_IPV6, REP_SUCCEEDED, SOCKS5_VERSION, Target, TargetAddr,
        resolve_fake_ip,
    },
};

/// 单会话待发队列长度（握手期间缓冲数据报，UDP 无重传，不能直接丢）。
const SESSION_QUEUE: usize = 64;
/// 单会话空闲超时：上行与下行均静默超过此时长则释放连接。
const SESSION_IDLE: Duration = Duration::from_secs(60);
/// UDP 数据报上限。
const MAX_DATAGRAM: usize = 65_535;

type SessionKey = (Target, String);
type Sessions = Arc<Mutex<HashMap<SessionKey, mpsc::Sender<Vec<u8>>>>>;

/// 处理一条 SOCKS5 UDP ASSOCIATE 控制连接。
///
/// 控制连接保持打开期间关联有效；任一方向关闭即结束并释放所有会话。
///
/// # Errors
///
/// 绑定本地 UDP 端口失败或写响应失败时返回错误。
pub async fn serve_associate(control: TcpStream, context: Arc<AppContext>) -> Result<()> {
    let local_ip = control
        .local_addr()
        .context("socks5 control local addr")?
        .ip();
    let udp = Arc::new(
        UdpSocket::bind(SocketAddr::new(local_ip, 0))
            .await
            .context("bind socks5 udp socket")?,
    );
    let bound = udp.local_addr().context("socks5 udp local addr")?;

    let (mut control_r, mut control_w) = tokio::io::split(control);
    control_w
        .write_all(&associate_reply(bound))
        .await
        .context("write udp associate reply")?;
    control_w.flush().await?;
    tracing::debug!(%bound, "socks5 udp associate established");

    let sessions: Sessions = Arc::new(Mutex::new(HashMap::new()));
    let mut client: Option<SocketAddr> = None;
    let mut buffer = vec![0u8; MAX_DATAGRAM];
    let mut discard = [0u8; 1];

    loop {
        let received = tokio::select! {
            result = udp.recv_from(&mut buffer) => result,
            result = control_r.read(&mut discard) => {
                match result {
                    // 控制连接关闭（或异常）即结束关联。
                    Ok(0) | Err(_) => break,
                    Ok(_) => continue,
                }
            }
        };
        let (len, source) = match received {
            Ok(value) => value,
            Err(error) => {
                tracing::debug!(%error, "udp associate recv failed");
                break;
            }
        };
        let client_addr = *client.get_or_insert(source);
        let Some((target, payload)) = parse_request(&buffer[..len]) else {
            tracing::debug!(len, "drop malformed socks5 udp datagram");
            continue;
        };
        let relay = Relay {
            udp: Arc::clone(&udp),
            client: client_addr,
            sessions: Arc::clone(&sessions),
            context: Arc::clone(&context),
        };
        if let Err(error) = relay.dispatch(target, payload).await {
            tracing::debug!(%error, "udp dispatch failed");
        }
    }

    sessions.lock().await.clear();
    Ok(())
}

/// 一个 UDP 关联的转发上下文。
struct Relay {
    udp: Arc<UdpSocket>,
    client: SocketAddr,
    sessions: Sessions,
    context: Arc<AppContext>,
}

/// 会话的出站类型。
enum SessionKind {
    Proxy(Box<Proxy>),
    Direct,
}

/// 会话的下行回写参数。
#[derive(Clone)]
struct Downlink {
    socket: Arc<UdpSocket>,
    client: SocketAddr,
    header: Vec<u8>,
}

impl Relay {
    async fn dispatch(&self, mut target: Target, payload: &[u8]) -> Result<()> {
        let rule_target = resolve_fake_ip(&mut target, &self.context.fakeip);
        let route = self.context.router().route(&rule_target)?;
        let (key, kind) = match route {
            Route::Reject => {
                tracing::debug!(dest = ?target, "udp reject");
                return Ok(());
            }
            Route::Proxy(proxy) => {
                let key = (target.clone(), proxy.name.clone());
                (key, SessionKind::Proxy(proxy))
            }
            Route::Direct => (
                (target.clone(), String::from("DIRECT")),
                SessionKind::Direct,
            ),
        };
        let sender = self.sender_for(key, kind, &target).await?;
        sender
            .send(payload.to_vec())
            .await
            .context("udp session closed")
    }

    /// 取出（必要时创建）该会话的发送端。
    async fn sender_for(
        &self,
        key: SessionKey,
        kind: SessionKind,
        target: &Target,
    ) -> Result<mpsc::Sender<Vec<u8>>> {
        if let Some(existing) = self.sessions.lock().await.get(&key).cloned() {
            return Ok(existing);
        }
        let Some(header) = reply_header(target) else {
            bail!("udp target address too long");
        };
        let (tx, rx) = mpsc::channel(SESSION_QUEUE);
        {
            let mut guard = self.sessions.lock().await;
            // 双检：并发首包只允许一个会话建立。
            if let Some(existing) = guard.get(&key).cloned() {
                return Ok(existing);
            }
            guard.insert(key.clone(), tx.clone());
        }

        let downlink = Downlink {
            socket: Arc::clone(&self.udp),
            client: self.client,
            header,
        };
        let sessions = Arc::clone(&self.sessions);
        let session_key = key.clone();
        let target = target.clone();
        tokio::spawn(async move {
            let result = match kind {
                SessionKind::Proxy(proxy) => run_proxy(*proxy, target, rx, downlink).await,
                SessionKind::Direct => run_direct(target, rx, downlink).await,
            };
            if let Err(error) = result {
                tracing::debug!(%error, "udp session ended");
            }
            sessions.lock().await.remove(&session_key);
        });
        Ok(tx)
    }
}

/// 代理会话：VLESS UDP 收发。
async fn run_proxy(
    proxy: Proxy,
    target: Target,
    mut rx: mpsc::Receiver<Vec<u8>>,
    downlink: Downlink,
) -> Result<()> {
    let (plain, pumps) = outbound::connect_proxy_udp(&proxy, &target)
        .await
        .context("vless udp connect")?;
    tracing::debug!(dest = ?target, proxy = %proxy.name, "udp session established");
    let (reader, mut writer) = tokio::io::split(plain);
    let activity = Arc::new(AtomicU64::new(now_millis()));
    let downstream = spawn_vless_downstream(reader, downlink, Arc::clone(&activity));

    let result = pump_vless_upstream(&mut writer, &mut rx, &activity).await;
    downstream.abort();
    for pump in pumps {
        pump.abort();
    }
    result
}

/// 下行任务：独占 reader，读到完整帧后再回写客户端。
fn spawn_vless_downstream<R>(
    mut reader: R,
    downlink: Downlink,
    activity: Arc<AtomicU64>,
) -> tokio::task::JoinHandle<()>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut buffer = vec![0u8; MAX_DATAGRAM];
        while let Ok(value) = reader.read_u16().await {
            let size = usize::from(value);
            if size == 0 {
                continue;
            }
            if reader.read_exact(&mut buffer[..size]).await.is_err() {
                break;
            }
            activity.store(now_millis(), Ordering::Relaxed);
            deliver(&downlink, &buffer[..size]).await;
        }
    })
}

/// 上行：待发队列 → `length + payload`，静默超时后检查下行是否也静默。
async fn pump_vless_upstream<W>(
    writer: &mut W,
    rx: &mut mpsc::Receiver<Vec<u8>>,
    activity: &Arc<AtomicU64>,
) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    loop {
        let incoming = match timeout(SESSION_IDLE, rx.recv()).await {
            Ok(Some(payload)) => payload,
            Ok(None) => break,
            Err(_) => {
                if idle_for(activity) >= SESSION_IDLE {
                    break;
                }
                continue;
            }
        };
        let size = u16::try_from(incoming.len()).unwrap_or(u16::MAX);
        writer
            .write_all(&size.to_be_bytes())
            .await
            .context("write udp length")?;
        writer
            .write_all(&incoming[..usize::from(size)])
            .await
            .context("write udp payload")?;
        writer.flush().await?;
        activity.store(now_millis(), Ordering::Relaxed);
    }
    Ok(())
}

/// 直连会话：系统 UDP socket 收发。
async fn run_direct(
    target: Target,
    mut rx: mpsc::Receiver<Vec<u8>>,
    downlink: Downlink,
) -> Result<()> {
    let socket = Arc::new(
        UdpSocket::bind("0.0.0.0:0")
            .await
            .context("bind direct udp socket")?,
    );
    match &target.addr {
        TargetAddr::Domain(domain) => socket
            .connect((domain.as_str(), target.port))
            .await
            .context("connect direct udp")?,
        TargetAddr::Ip(ip) => socket
            .connect((*ip, target.port))
            .await
            .context("connect direct udp")?,
    }
    tracing::debug!(dest = ?target, "udp direct session established");

    let activity = Arc::new(AtomicU64::new(now_millis()));
    let down_socket = Arc::clone(&socket);
    let down_activity = Arc::clone(&activity);
    let down_link = downlink.clone();
    let downstream = tokio::spawn(async move {
        let mut buffer = vec![0u8; MAX_DATAGRAM];
        loop {
            let Ok(read) = down_socket.recv(&mut buffer).await else {
                break;
            };
            if read == 0 {
                continue;
            }
            down_activity.store(now_millis(), Ordering::Relaxed);
            deliver(&down_link, &buffer[..read]).await;
        }
    });

    loop {
        let incoming = match timeout(SESSION_IDLE, rx.recv()).await {
            Ok(Some(payload)) => payload,
            Ok(None) => break,
            Err(_) => {
                if idle_for(&activity) >= SESSION_IDLE {
                    break;
                }
                continue;
            }
        };
        if socket.send(&incoming).await.is_err() {
            break;
        }
        activity.store(now_millis(), Ordering::Relaxed);
    }
    downstream.abort();
    Ok(())
}

/// 把载荷加上 SOCKS5 UDP 头回写给客户端。
async fn deliver(downlink: &Downlink, payload: &[u8]) {
    let mut packet = Vec::with_capacity(downlink.header.len() + payload.len());
    packet.extend_from_slice(&downlink.header);
    packet.extend_from_slice(payload);
    if downlink
        .socket
        .send_to(&packet, downlink.client)
        .await
        .is_err()
    {
        tracing::debug!("udp deliver to client failed");
    }
}

/// 当前 Unix 毫秒时间戳。
fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

/// 距上次活动已过去的时长。
fn idle_for(activity: &AtomicU64) -> Duration {
    let idle_ms = u64::try_from(SESSION_IDLE.as_millis()).unwrap_or(u64::MAX);
    let last = activity.load(Ordering::Relaxed);
    let elapsed = now_millis().saturating_sub(last);
    Duration::from_millis(elapsed.min(idle_ms.saturating_mul(2)))
}

/// 解析 SOCKS5 UDP 请求数据报，返回目标与载荷。
///
/// 布局：`RSV(2) FRAG(1) ATYP(1) DST.ADDR DST.PORT(2) DATA`。
/// `FRAG != 0`（分片）不支持，直接丢弃。
fn parse_request(data: &[u8]) -> Option<(Target, &[u8])> {
    if data.len() < 4 || data[0] != 0 || data[1] != 0 || data[2] != 0 {
        return None;
    }
    let (addr, mut offset) = match data[3] {
        ATYP_IPV4 => {
            if data.len() < 8 {
                return None;
            }
            let mut octets = [0u8; 4];
            octets.copy_from_slice(&data[4..8]);
            (TargetAddr::Ip(IpAddr::V4(octets.into())), 8)
        }
        ATYP_IPV6 => {
            if data.len() < 20 {
                return None;
            }
            let mut octets = [0u8; 16];
            octets.copy_from_slice(&data[4..20]);
            (TargetAddr::Ip(IpAddr::V6(octets.into())), 20)
        }
        ATYP_DOMAIN => {
            if data.len() < 5 {
                return None;
            }
            let len = usize::from(data[4]);
            if data.len() < 5 + len + 2 {
                return None;
            }
            let domain = std::str::from_utf8(&data[5..5 + len]).ok()?.to_owned();
            (TargetAddr::Domain(domain), 5 + len)
        }
        _ => return None,
    };
    if data.len() < offset + 2 {
        return None;
    }
    let port = u16::from_be_bytes([data[offset], data[offset + 1]]);
    offset += 2;
    Some((Target { addr, port }, &data[offset..]))
}

/// 构造回写用的 SOCKS5 UDP 头。
fn reply_header(target: &Target) -> Option<Vec<u8>> {
    let mut data = vec![0x00, 0x00, 0x00];
    match &target.addr {
        TargetAddr::Ip(IpAddr::V4(ip)) => {
            data.push(ATYP_IPV4);
            data.extend_from_slice(&ip.octets());
        }
        TargetAddr::Ip(IpAddr::V6(ip)) => {
            data.push(ATYP_IPV6);
            data.extend_from_slice(&ip.octets());
        }
        TargetAddr::Domain(domain) => {
            let len = u8::try_from(domain.len()).ok()?;
            data.push(ATYP_DOMAIN);
            data.push(len);
            data.extend_from_slice(domain.as_bytes());
        }
    }
    data.extend_from_slice(&target.port.to_be_bytes());
    Some(data)
}

/// 构造 UDP ASSOCIATE 成功响应（携带实际绑定的地址）。
fn associate_reply(bound: SocketAddr) -> Vec<u8> {
    let mut data = vec![SOCKS5_VERSION, REP_SUCCEEDED, 0x00];
    match bound.ip() {
        IpAddr::V4(ip) => {
            data.push(ATYP_IPV4);
            data.extend_from_slice(&ip.octets());
        }
        IpAddr::V6(ip) => {
            data.push(ATYP_IPV6);
            data.extend_from_slice(&ip.octets());
        }
    }
    data.extend_from_slice(&bound.port().to_be_bytes());
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ipv4_datagram() {
        let mut data = vec![0, 0, 0, ATYP_IPV4, 1, 2, 3, 4];
        data.extend_from_slice(&53u16.to_be_bytes());
        data.extend_from_slice(b"payload");
        let (target, payload) = parse_request(&data).unwrap();
        assert_eq!(target.port, 53);
        assert_eq!(target.addr, TargetAddr::Ip("1.2.3.4".parse().unwrap()));
        assert_eq!(payload, b"payload");
    }

    #[test]
    fn parse_domain_datagram_roundtrip() {
        let mut data = vec![0, 0, 0, ATYP_DOMAIN, 11];
        data.extend_from_slice(b"example.com");
        data.extend_from_slice(&443u16.to_be_bytes());
        data.extend_from_slice(b"x");
        let (target, payload) = parse_request(&data).unwrap();
        assert_eq!(target.addr, TargetAddr::Domain("example.com".into()));
        assert_eq!(target.port, 443);
        assert_eq!(payload, b"x");

        let header = reply_header(&target).unwrap();
        assert_eq!(header, data[..data.len() - 1].to_vec());
    }

    #[test]
    fn reject_fragmented_and_truncated() {
        // FRAG != 0
        assert!(parse_request(&[0, 0, 1, ATYP_IPV4, 1, 2, 3, 4, 0, 53]).is_none());
        // IPv4 但数据不足
        assert!(parse_request(&[0, 0, 0, ATYP_IPV4, 1, 2]).is_none());
        // 域名长度超出剩余数据
        assert!(parse_request(&[0, 0, 0, ATYP_DOMAIN, 200, b'a']).is_none());
        // 缺少端口
        assert!(parse_request(&[0, 0, 0, ATYP_IPV4, 1, 2, 3, 4, 0]).is_none());
    }

    #[test]
    fn associate_reply_encodes_bound_port() {
        let reply = associate_reply("127.0.0.1:1081".parse().unwrap());
        assert_eq!(reply[0], SOCKS5_VERSION);
        assert_eq!(reply[1], REP_SUCCEEDED);
        assert_eq!(reply[3], ATYP_IPV4);
        assert_eq!(u16::from_be_bytes([reply[8], reply[9]]), 1081);
    }
}
