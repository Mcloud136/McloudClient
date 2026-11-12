//! 最小 SOCKS5 服务端（CONNECT / UDP ASSOCIATE）。
//!
//! 只实现 PoC 需要的最小子集：无认证、CONNECT、UDP ASSOCIATE、
//! IPv4 / 域名 / IPv6 目标。BIND 明确不支持。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::{
    context::AppContext, dns::FakeIp, metrics, outbound, relay, router::Route,
    rules::Target as RuleTarget, udp,
};

/// SOCKS5 目标地址。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TargetAddr {
    Ip(IpAddr),
    Domain(String),
}

/// SOCKS5 目标（地址 + 端口）。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Target {
    pub addr: TargetAddr,
    pub port: u16,
}

pub(crate) const SOCKS5_VERSION: u8 = 0x05;
pub(crate) const SOCKS5_NO_AUTH: u8 = 0x00;
const SOCKS5_CMD_CONNECT: u8 = 0x01;
const SOCKS5_CMD_UDP_ASSOCIATE: u8 = 0x03;
pub(crate) const ATYP_IPV4: u8 = 0x01;
pub(crate) const ATYP_DOMAIN: u8 = 0x03;
pub(crate) const ATYP_IPV6: u8 = 0x04;
pub(crate) const REP_SUCCEEDED: u8 = 0x00;
const REP_GENERAL_FAILURE: u8 = 0x01;

/// 处理一条 SOCKS5 连接。
pub async fn serve(mut stream: TcpStream, context: Arc<AppContext>) -> Result<()> {
    let _active = metrics::on_accept(&context.metrics);
    stream.set_nodelay(true).context("set nodelay")?;
    handshake(&mut stream).await?;
    let (command, mut target) = read_request(&mut stream).await?;
    if command == SOCKS5_CMD_UDP_ASSOCIATE {
        return udp::serve_associate(stream, context).await;
    }
    let rule_target = resolve_fake_ip(&mut target, &context.fakeip);
    let route = context.router().route(&rule_target)?;
    let connected = match route {
        Route::Proxy(proxy) => {
            tracing::debug!(dest = ?rule_target, proxy = %proxy.name, "socks5 route");
            outbound::connect_proxy(&proxy, &target).await
        }
        Route::Direct => {
            tracing::debug!(dest = ?rule_target, "socks5 direct");
            outbound::connect_direct(&target)
                .await
                .map(|stream| (stream, Vec::new()))
        }
        Route::Reject => {
            tracing::debug!(dest = ?rule_target, "socks5 reject");
            stream.write_all(&failure_reply()).await?;
            return Ok(());
        }
    };

    let (plain, pumps) = match connected {
        Ok(value) => value,
        Err(error) => {
            // 尽力告知客户端失败，然后关闭。
            let _ = stream.write_all(&failure_reply()).await;
            return Err(error);
        }
    };

    stream.write_all(&success_reply()).await?;
    stream.flush().await?;

    let result = relay::bidirectional(stream, plain, &context.metrics).await;
    for pump in pumps {
        pump.abort();
    }
    result
}

async fn handshake(stream: &mut TcpStream) -> Result<()> {
    let mut head = [0u8; 2];
    stream
        .read_exact(&mut head)
        .await
        .context("read socks5 greeting")?;
    if head[0] != SOCKS5_VERSION {
        bail!("unsupported socks version {}", head[0]);
    }
    let mut methods = vec![0u8; usize::from(head[1])];
    stream
        .read_exact(&mut methods)
        .await
        .context("read socks5 methods")?;
    if !methods.contains(&SOCKS5_NO_AUTH) {
        bail!("socks5 client does not offer no-auth");
    }
    stream
        .write_all(&[SOCKS5_VERSION, SOCKS5_NO_AUTH])
        .await
        .context("write socks5 method")?;
    Ok(())
}

async fn read_request(stream: &mut TcpStream) -> Result<(u8, Target)> {
    let mut head = [0u8; 4];
    stream
        .read_exact(&mut head)
        .await
        .context("read socks5 request")?;
    if head[0] != SOCKS5_VERSION {
        bail!("unsupported socks version {}", head[0]);
    }
    if head[1] != SOCKS5_CMD_CONNECT && head[1] != SOCKS5_CMD_UDP_ASSOCIATE {
        bail!("unsupported socks5 command {}", head[1]);
    }
    let addr = read_addr(stream, head[3]).await?;
    let mut port = [0u8; 2];
    stream
        .read_exact(&mut port)
        .await
        .context("read socks5 port")?;
    Ok((
        head[1],
        Target {
            addr,
            port: u16::from_be_bytes(port),
        },
    ))
}

async fn read_addr(stream: &mut TcpStream, atyp: u8) -> Result<TargetAddr> {
    match atyp {
        ATYP_IPV4 => {
            let mut octets = [0u8; 4];
            stream
                .read_exact(&mut octets)
                .await
                .context("read socks5 ipv4")?;
            Ok(TargetAddr::Ip(IpAddr::V4(Ipv4Addr::from(octets))))
        }
        ATYP_DOMAIN => {
            let mut len = [0u8; 1];
            stream
                .read_exact(&mut len)
                .await
                .context("read socks5 domain length")?;
            let mut domain = vec![0u8; usize::from(len[0])];
            stream
                .read_exact(&mut domain)
                .await
                .context("read socks5 domain")?;
            let domain = String::from_utf8(domain).context("socks5 domain is not utf-8")?;
            Ok(TargetAddr::Domain(domain))
        }
        ATYP_IPV6 => {
            let mut octets = [0u8; 16];
            stream
                .read_exact(&mut octets)
                .await
                .context("read socks5 ipv6")?;
            Ok(TargetAddr::Ip(IpAddr::V6(Ipv6Addr::from(octets))))
        }
        other => bail!("unsupported socks5 address type {other}"),
    }
}

pub(crate) fn rule_target(target: &Target) -> RuleTarget {
    match &target.addr {
        TargetAddr::Domain(domain) => RuleTarget::Domain(domain.to_ascii_lowercase()),
        TargetAddr::Ip(ip) => RuleTarget::Ip(*ip),
    }
}

/// fake-ip 目标还原成域名：既让域名规则（`GEOSITE`）生效，也保证节点侧
/// 按域名解析（`ATYP=DOMAIN`），避免客户端本地解析。
pub(crate) fn resolve_fake_ip(target: &mut Target, fakeip: &FakeIp) -> RuleTarget {
    let candidate = match &target.addr {
        TargetAddr::Ip(IpAddr::V4(ip)) => Some(*ip),
        _ => None,
    };
    if let Some(ip) = candidate
        && let Some(domain) = fakeip.lookup(ip)
    {
        tracing::debug!(%ip, %domain, "fake-ip resolved");
        target.addr = TargetAddr::Domain(domain.clone());
        return RuleTarget::Domain(domain);
    }
    rule_target(target)
}

fn success_reply() -> [u8; 10] {
    // VER REP RSV ATYP=IPv4 BND.ADDR=0.0.0.0 BND.PORT=0
    [
        SOCKS5_VERSION,
        REP_SUCCEEDED,
        0x00,
        ATYP_IPV4,
        0,
        0,
        0,
        0,
        0,
        0,
    ]
}

fn failure_reply() -> [u8; 10] {
    [
        SOCKS5_VERSION,
        REP_GENERAL_FAILURE,
        0x00,
        ATYP_IPV4,
        0,
        0,
        0,
        0,
        0,
        0,
    ]
}
