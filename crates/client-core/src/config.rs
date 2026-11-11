//! CLI 配置。
//!
//! 两种模式：
//! - `--profile <json>`：加载 Clash 风格 profile（由 `script/clash2profile.py` 从 YAML 转换）。
//! - `--server/--uuid`：单节点模式，内部合成一个只含 `MATCH` 规则的 profile。

use std::{collections::HashMap, net::SocketAddr, path::PathBuf};

use anyhow::{Context, Result};
use clap::Parser;

use crate::{
    geo::GeoData,
    profile::{Profile, Proxy, WsOpts},
};

/// 最小 Rust 客户端数据面：SOCKS5 入站 -> 规则分流 -> VLESS/WS/TLS/mlkem 出站。
#[derive(Debug, Clone, Parser)]
#[command(name = "client-core", version, about)]
pub struct Config {
    /// 本地 SOCKS5 监听地址。
    #[arg(long, default_value = "127.0.0.1:1080")]
    pub listen: String,

    /// 加载 Clash 风格 JSON profile。
    #[arg(long)]
    pub profile: Option<String>,

    /// 单节点模式：节点入口 `host:port`。
    #[arg(long, required_unless_present_any = ["profile", "profile_url"])]
    pub server: Option<String>,

    /// VLESS 用户 UUID。
    #[arg(long, required_unless_present_any = ["profile", "profile_url"])]
    pub uuid: Option<String>,

    /// 加密串，如 `mlkem768x25519plus.native.1rtt.<public>`；缺省为明文。
    #[arg(long)]
    pub encryption: Option<String>,

    /// WS 请求路径。
    #[arg(long, default_value = "/cdn-api")]
    pub path: String,

    /// WS Host 头；缺省取 server 的 host 部分。
    #[arg(long)]
    pub host: Option<String>,

    /// 跳过 TLS 证书校验（仅测试用）。
    #[arg(long, default_value_t = false)]
    pub insecure: bool,

    /// GeoIP CN 数据文件（每行一个 CIDR）。
    #[arg(long)]
    pub geoip: Option<PathBuf>,

    /// GeoSite CN 数据文件（v2fly domain-list 文本格式）。
    #[arg(long)]
    pub geosite: Option<PathBuf>,

    /// 启用 fake-ip DNS 服务（UDP），例如 `127.0.0.1:5353`。
    #[arg(long)]
    pub dns_listen: Option<String>,

    /// fake-ip 模式下 `DIRECT` 域名使用的上游 DNS。
    #[arg(long, default_value = "223.5.5.5:53")]
    pub dns_upstream: String,

    /// 订阅/配置 URL（须返回 JSON profile，YAML 需先经 clash2profile.py 转换）。
    #[arg(long)]
    pub profile_url: Option<String>,

    /// 配置热重载间隔（秒）；0 表示禁用。
    #[arg(long, default_value_t = 0)]
    pub reload_interval: u64,

    /// IPC 命名管道名（如 `\\.\pipe\mcloud-client`）；缺省不启用控制面接口。
    #[arg(long)]
    pub ipc_pipe: Option<String>,
}

impl Config {
    /// 构造 profile：优先读文件，否则用单节点参数合成。
    ///
    /// # Errors
    ///
    /// profile 读取/解析失败，或单节点参数缺失/端口非法时返回错误。
    pub fn build_profile(&self) -> Result<Profile> {
        if let Some(path) = &self.profile {
            let data =
                std::fs::read_to_string(path).with_context(|| format!("read profile {path}"))?;
            return serde_json::from_str(&data).with_context(|| format!("parse profile {path}"));
        }
        let server = self.server.as_deref().context("--server is required")?;
        let uuid = self.uuid.as_deref().context("--uuid is required")?;
        let (host, port) = split_host_port(server)?;
        let ws_host = self.host.clone().unwrap_or_else(|| host.clone());
        let proxy = Proxy {
            name: "default".to_owned(),
            kind: "vless".to_owned(),
            server: host,
            port,
            uuid: uuid.to_owned(),
            network: "ws".to_owned(),
            tls: true,
            servername: Some(ws_host.clone()),
            encryption: self.encryption.clone(),
            ws_opts: Some(WsOpts {
                path: self.path.clone(),
                headers: HashMap::from([("Host".to_owned(), ws_host)]),
            }),
            skip_cert_verify: self.insecure,
        };
        Ok(Profile {
            proxies: vec![proxy],
            proxy_groups: Vec::new(),
            rule_providers: HashMap::new(),
            rules: vec!["MATCH,default".to_owned()],
        })
    }

    /// 加载 GEOIP/GEOSITE 数据。
    ///
    /// # Errors
    ///
    /// 数据文件读取失败时返回错误。
    pub fn build_geo(&self) -> Result<GeoData> {
        GeoData::load(self.geoip.as_ref(), self.geosite.as_ref())
    }

    /// 解析上游 DNS 地址。
    ///
    /// # Errors
    ///
    /// `--dns-upstream` 不是合法的 `ip:port` 时返回错误。
    pub fn dns_upstream_addr(&self) -> Result<SocketAddr> {
        self.dns_upstream
            .parse()
            .with_context(|| format!("invalid --dns-upstream {}", self.dns_upstream))
    }
}

fn split_host_port(server: &str) -> Result<(String, u16)> {
    let rest = server
        .trim_start_matches("wss://")
        .trim_start_matches("ws://");
    let (host, port) = rest
        .rsplit_once(':')
        .context("--server must be host:port")?;
    let port: u16 = port.parse().context("invalid --server port")?;
    Ok((host.to_owned(), port))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(server: &str) -> Config {
        Config {
            listen: "127.0.0.1:1080".to_owned(),
            profile: None,
            server: Some(server.to_owned()),
            uuid: Some("11111111-1111-1111-1111-111111111111".to_owned()),
            encryption: Some("mlkem768x25519plus.native.1rtt.abc".to_owned()),
            path: "/cdn-api".to_owned(),
            host: None,
            insecure: false,
            geoip: None,
            geosite: None,
            dns_listen: None,
            dns_upstream: "223.5.5.5:53".to_owned(),
            profile_url: None,
            reload_interval: 0,
            ipc_pipe: None,
        }
    }

    #[test]
    fn synthetic_profile_has_match_rule() {
        let profile = config("hk2.example.com:443").build_profile().unwrap();
        assert_eq!(profile.proxies.len(), 1);
        assert_eq!(profile.proxies[0].server, "hk2.example.com");
        assert_eq!(profile.proxies[0].port, 443);
        assert_eq!(profile.proxies[0].ws_path(), "/cdn-api");
        assert_eq!(profile.rules, vec!["MATCH,default".to_owned()]);
    }

    #[test]
    fn rejects_missing_port() {
        assert!(config("hk2.example.com").build_profile().is_err());
    }
}
