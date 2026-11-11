//! fake-ip DNS 服务与域名反查。
//!
//! 解决的问题：SOCKS5 只拿到 IP 时，基于域名的规则（`GEOSITE`）失效，
//! 只能按 `GEOIP` 判断，且客户端必须真实解析域名（易被污染）。
//!
//! 做法（业界通用的 fake-ip 模式）：
//!
//! 1. 应用把域名查询发给本模块；
//! 2. 域名先过规则：
//!    - 命中 `DIRECT` → 转发给直连上游 DNS，返回**真实 IP**；
//!    - 命中 `PROXY` → 立即分配一个 `198.18.0.0/15` 的**假 IP** 返回，
//!      不查询上游（避免污染，也省一次往返）；
//!    - 命中 `REJECT` → 返回 NXDOMAIN；
//! 3. 应用连假 IP 时，SOCKS5/UDP 层反查出域名，再按域名发往节点
//!    （`ATYP=DOMAIN`，由节点侧解析）。
//!
//! 非 `A` 查询（`AAAA`/`HTTPS` 等）在代理域名上一律返回空答案，促使
//! 应用回退到 `A`，这是 fake-ip 的标准行为。

use std::{
    collections::HashMap,
    net::{Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result};
use tokio::{net::UdpSocket, time::timeout};

use crate::{context::AppContext, router::Route, rules::Target as RuleTarget};

/// fake-ip 网段首字节（`198.18.0.0/15`，RFC 2544 保留段）。
const FAKEIP_FIRST: u8 = 198;
/// fake-ip 网段第二字节。
const FAKEIP_SECOND: u8 = 18;
/// 单段 `/16` 可用地址数（去掉网络号与保留）。
const FAKEIP_CAPACITY: u32 = 65_534;
/// 假 IP 的 TTL：短一些，便于映射变更后快速生效。
const FAKEIP_TTL: u32 = 60;
/// 上游 DNS 查询超时。
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(5);
/// DNS 报文上限。
const MAX_DNS_PACKET: usize = 4096;

const TYPE_A: u16 = 1;
const RCODE_NOERROR: u8 = 0;
const RCODE_NXDOMAIN: u8 = 3;

/// 域名 ↔ 假 IP 的双向映射池。
#[derive(Debug)]
pub struct FakeIpPool {
    domain_to_ip: HashMap<String, Ipv4Addr>,
    ip_to_domain: HashMap<Ipv4Addr, String>,
    next: u32,
}

impl Default for FakeIpPool {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeIpPool {
    /// 创建空池。
    #[must_use]
    pub fn new() -> Self {
        Self {
            domain_to_ip: HashMap::new(),
            ip_to_domain: HashMap::new(),
            next: 1,
        }
    }

    /// 地址是否落在 fake-ip 网段。
    #[must_use]
    pub fn is_fake(ip: Ipv4Addr) -> bool {
        let octets = ip.octets();
        octets[0] == FAKEIP_FIRST
            && (octets[1] == FAKEIP_SECOND || octets[1] == FAKEIP_SECOND.saturating_add(1))
    }

    /// 为域名分配假 IP（已分配则复用）。池满后循环覆盖最旧的条目。
    pub fn assign(&mut self, domain: &str) -> Ipv4Addr {
        if let Some(ip) = self.domain_to_ip.get(domain) {
            return *ip;
        }
        let base = u32::from_be_bytes([FAKEIP_FIRST, FAKEIP_SECOND, 0, 0]);
        let ip = Ipv4Addr::from(base.saturating_add(self.next));
        self.next = if self.next >= FAKEIP_CAPACITY {
            1
        } else {
            self.next + 1
        };
        if let Some(previous) = self.ip_to_domain.remove(&ip) {
            self.domain_to_ip.remove(&previous);
        }
        self.domain_to_ip.insert(domain.to_owned(), ip);
        self.ip_to_domain.insert(ip, domain.to_owned());
        ip
    }

    /// 反查假 IP 对应的域名。
    #[must_use]
    pub fn lookup(&self, ip: Ipv4Addr) -> Option<&str> {
        self.ip_to_domain.get(&ip).map(String::as_str)
    }

    /// 已分配的域名数量。
    #[must_use]
    pub fn len(&self) -> usize {
        self.domain_to_ip.len()
    }

    /// 是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.domain_to_ip.is_empty()
    }
}

/// 线程安全的 fake-ip 池（锁只在同步映射操作内持有，不跨 `await`）。
#[derive(Debug, Default)]
pub struct FakeIp {
    inner: Mutex<FakeIpPool>,
}

impl FakeIp {
    /// 分配假 IP；锁中毒时退化为 `0.0.0.0`（调用方会看到无效地址）。
    #[must_use]
    pub fn assign(&self, domain: &str) -> Ipv4Addr {
        self.inner
            .lock()
            .map(|mut pool| pool.assign(domain))
            .unwrap_or(Ipv4Addr::UNSPECIFIED)
    }

    /// 反查域名；非 fake-ip 段直接返回 `None`（避免不必要的加锁与查表）。
    #[must_use]
    pub fn lookup(&self, ip: Ipv4Addr) -> Option<String> {
        if !FakeIpPool::is_fake(ip) {
            return None;
        }
        self.inner
            .lock()
            .ok()
            .and_then(|pool| pool.lookup(ip).map(str::to_owned))
    }

    /// 已分配的域名数量。
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.lock().map(|pool| pool.len()).unwrap_or_default()
    }

    /// 是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner
            .lock()
            .map(|pool| pool.is_empty())
            .unwrap_or(true)
    }
}

/// DNS 查询中的 question 段。
#[derive(Debug, Clone)]
struct Question {
    name: String,
    qtype: u16,
    /// question 段结束位置（响应里原样回填这一段）。
    end: usize,
}

/// fake-ip DNS 服务器。
pub struct DnsServer {
    context: Arc<AppContext>,
    upstream: SocketAddr,
}

impl DnsServer {
    /// 构造服务器。
    #[must_use]
    pub fn new(context: Arc<AppContext>, upstream: SocketAddr) -> Self {
        Self { context, upstream }
    }

    /// 在给定 UDP socket 上服务，永不返回（除非 socket 出错）。
    pub async fn serve(self: Arc<Self>, socket: Arc<UdpSocket>) {
        let mut buffer = vec![0u8; MAX_DNS_PACKET];
        loop {
            let Ok((len, peer)) = socket.recv_from(&mut buffer).await else {
                tracing::debug!("dns socket closed");
                return;
            };
            let query = buffer[..len].to_vec();
            let server = Arc::clone(&self);
            let out = Arc::clone(&socket);
            drop(tokio::spawn(async move {
                if let Some(response) = server.handle(&query).await
                    && out.send_to(&response, peer).await.is_err()
                {
                    tracing::debug!(%peer, "dns response send failed");
                }
            }));
        }
    }

    /// 处理一条查询；无法解析时返回 `None`（静默丢弃）。
    async fn handle(&self, query: &[u8]) -> Option<Vec<u8>> {
        let question = parse_question(query)?;
        let route = self
            .context
            .router()
            .route(&RuleTarget::Domain(question.name.clone()))
            .ok()?;
        match route {
            Route::Direct => {
                tracing::debug!(domain = %question.name, "dns direct forward");
                self.forward(query).await
            }
            Route::Reject => {
                tracing::debug!(domain = %question.name, "dns reject");
                Some(empty_response(query, question.end, RCODE_NXDOMAIN))
            }
            Route::Proxy(_) => {
                if question.qtype != TYPE_A {
                    // AAAA / HTTPS 等：给空答案，促使应用回退到 A。
                    return Some(empty_response(query, question.end, RCODE_NOERROR));
                }
                let ip = self.context.fakeip.assign(&question.name);
                tracing::debug!(domain = %question.name, %ip, "dns fake-ip");
                Some(fakeip_response(query, question.end, ip))
            }
        }
    }

    /// 转发给直连上游 DNS。
    async fn forward(&self, query: &[u8]) -> Option<Vec<u8>> {
        let socket = UdpSocket::bind("0.0.0.0:0").await.ok()?;
        socket.connect(self.upstream).await.ok()?;
        socket.send(query).await.ok()?;
        let mut buffer = vec![0u8; MAX_DNS_PACKET];
        let len = timeout(UPSTREAM_TIMEOUT, socket.recv(&mut buffer))
            .await
            .ok()?
            .ok()?;
        buffer.truncate(len);
        Some(buffer)
    }
}

/// 解析 question 段（只支持单条 question、不支持压缩指针）。
fn parse_question(packet: &[u8]) -> Option<Question> {
    if packet.len() < 12 {
        return None;
    }
    let qdcount = u16::from_be_bytes([packet[4], packet[5]]);
    if qdcount != 1 {
        return None;
    }
    let mut offset = 12;
    let mut labels: Vec<String> = Vec::new();
    loop {
        let len = usize::from(*packet.get(offset)?);
        offset += 1;
        if len == 0 {
            break;
        }
        if len & 0xC0 != 0 || labels.len() >= 64 {
            return None;
        }
        let end = offset.checked_add(len)?;
        let label = packet.get(offset..end)?;
        labels.push(std::str::from_utf8(label).ok()?.to_ascii_lowercase());
        offset = end;
    }
    if labels.is_empty() {
        return None;
    }
    let qtype = u16::from_be_bytes([*packet.get(offset)?, *packet.get(offset + 1)?]);
    // 跳过 QTYPE(2) + QCLASS(2)
    offset = offset.checked_add(4)?;
    if offset > packet.len() {
        return None;
    }
    Some(Question {
        name: labels.join("."),
        qtype,
        end: offset,
    })
}

/// 构造带单条 A 记录的响应。
fn fakeip_response(query: &[u8], question_end: usize, ip: Ipv4Addr) -> Vec<u8> {
    let mut response = Vec::with_capacity(question_end + 16);
    response.extend_from_slice(&query[..2]); // ID
    response.extend_from_slice(&[0x81, 0x80]); // QR=1 RD=1 RA=1 NOERROR
    response.extend_from_slice(&[0x00, 0x01]); // QDCOUNT
    response.extend_from_slice(&[0x00, 0x01]); // ANCOUNT
    response.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // NSCOUNT + ARCOUNT
    response.extend_from_slice(&query[12..question_end]);
    response.extend_from_slice(&[0xC0, 0x0C]); // NAME: 指向 question
    response.extend_from_slice(&[0x00, 0x01, 0x00, 0x01]); // TYPE=A CLASS=IN
    response.extend_from_slice(&FAKEIP_TTL.to_be_bytes());
    response.extend_from_slice(&[0x00, 0x04]); // RDLENGTH
    response.extend_from_slice(&ip.octets());
    response
}

/// 构造无答案响应（用于 AAAA 与 REJECT）。
fn empty_response(query: &[u8], question_end: usize, rcode: u8) -> Vec<u8> {
    let mut response = Vec::with_capacity(question_end);
    response.extend_from_slice(&query[..2]); // ID
    response.extend_from_slice(&[0x81, 0x80 | rcode]); // QR=1 RD=1 RA=1 + RCODE
    response.extend_from_slice(&[0x00, 0x01]); // QDCOUNT
    response.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
    response.extend_from_slice(&query[12..question_end]);
    response
}

/// 绑定并启动 DNS 服务。
///
/// # Errors
///
/// 监听地址非法或绑定失败时返回错误。
pub async fn bind(listen: &str) -> Result<UdpSocket> {
    UdpSocket::bind(listen)
        .await
        .with_context(|| format!("bind dns {listen}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(name: &str, qtype: u16) -> Vec<u8> {
        let mut data = vec![0xAB, 0xCD, 0x01, 0x00, 0x00, 0x01];
        data.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        for label in name.split('.') {
            data.push(u8::try_from(label.len()).unwrap());
            data.extend_from_slice(label.as_bytes());
        }
        data.push(0);
        data.extend_from_slice(&qtype.to_be_bytes());
        data.extend_from_slice(&1u16.to_be_bytes());
        data
    }

    #[test]
    fn assigns_and_reuses_fake_ip() {
        let mut pool = FakeIpPool::new();
        let first = pool.assign("www.google.com");
        assert!(FakeIpPool::is_fake(first));
        assert_eq!(pool.assign("www.google.com"), first);
        assert_eq!(pool.lookup(first), Some("www.google.com"));
        assert_eq!(pool.len(), 1);

        let second = pool.assign("www.youtube.com");
        assert_ne!(first, second);
        assert_eq!(pool.len(), 2);
    }

    #[test]
    fn fake_range_detection() {
        assert!(FakeIpPool::is_fake("198.18.0.1".parse().unwrap()));
        assert!(FakeIpPool::is_fake("198.18.255.254".parse().unwrap()));
        assert!(FakeIpPool::is_fake("198.19.1.1".parse().unwrap()));
        assert!(!FakeIpPool::is_fake("198.20.0.1".parse().unwrap()));
        assert!(!FakeIpPool::is_fake("192.168.1.1".parse().unwrap()));
    }

    #[test]
    fn parses_question_section() {
        let packet = query("www.baidu.com", TYPE_A);
        let parsed = parse_question(&packet).unwrap();
        assert_eq!(parsed.name, "www.baidu.com");
        assert_eq!(parsed.qtype, TYPE_A);
        assert_eq!(parsed.end, packet.len());
    }

    #[test]
    fn rejects_malformed_question() {
        assert!(parse_question(&[]).is_none());
        assert!(parse_question(&[0u8; 11]).is_none());
        // QDCOUNT = 0
        let mut packet = query("a.com", TYPE_A);
        packet[4] = 0;
        packet[5] = 0;
        assert!(parse_question(&packet).is_none());
    }

    #[test]
    fn fakeip_response_shape() {
        let packet = query("www.google.com", TYPE_A);
        let parsed = parse_question(&packet).unwrap();
        let ip: Ipv4Addr = "198.18.0.7".parse().unwrap();
        let response = fakeip_response(&packet, parsed.end, ip);

        assert_eq!(&response[..2], &[0xAB, 0xCD]);
        assert_eq!(response[3] & 0x0F, RCODE_NOERROR);
        assert_eq!(u16::from_be_bytes([response[4], response[5]]), 1); // QDCOUNT
        assert_eq!(u16::from_be_bytes([response[6], response[7]]), 1); // ANCOUNT
        // 最后 4 字节是假 IP
        assert_eq!(&response[response.len() - 4..], &ip.octets());
        assert_eq!(response.len(), parsed.end + 16);
    }

    #[test]
    fn empty_response_has_no_answer() {
        let packet = query("www.google.com", 28);
        let parsed = parse_question(&packet).unwrap();
        let response = empty_response(&packet, parsed.end, RCODE_NOERROR);
        assert_eq!(u16::from_be_bytes([response[6], response[7]]), 0);
        assert_eq!(response.len(), parsed.end);
    }
}
