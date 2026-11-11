//! Clash 风格 profile（JSON）模型。
//!
//! Clash YAML 由 `script/clash2profile.py` 转成等价 JSON 后由本模块解析；
//! Rust 侧不引入 YAML 依赖（构建环境 crates.io 不可达）。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// 顶层 profile：代理、策略组、规则。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Profile {
    #[serde(default)]
    pub proxies: Vec<Proxy>,
    #[serde(rename = "proxy-groups", default)]
    pub proxy_groups: Vec<ProxyGroup>,
    #[serde(rename = "rule-providers", default)]
    pub rule_providers: HashMap<String, RuleProvider>,
    #[serde(default)]
    pub rules: Vec<String>,
}

/// 规则集（Clash `rule-providers` 的本地化形态）。
///
/// 上游的 `type: http` 由 `script/clash2profile.py` 预先下载并转换成
/// 纯文本规则文件，Rust 侧只读本地文件（构建环境无法稳定访问外网）。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RuleProvider {
    /// 规则集类型：`domain` / `ipcidr` / `classical`。
    pub behavior: String,
    /// 本地规则文件路径。
    pub path: String,
}

/// 单个出站（当前仅支持 VLESS + WS + TLS）。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Proxy {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub server: String,
    pub port: u16,
    #[serde(default)]
    pub uuid: String,
    #[serde(default)]
    pub network: String,
    #[serde(default)]
    pub tls: bool,
    #[serde(default)]
    pub servername: Option<String>,
    #[serde(default)]
    pub encryption: Option<String>,
    #[serde(rename = "ws-opts", default)]
    pub ws_opts: Option<WsOpts>,
    #[serde(rename = "skip-cert-verify", default)]
    pub skip_cert_verify: bool,
}

/// WebSocket 传输参数。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct WsOpts {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub headers: HashMap<String, String>,
}

/// 策略组。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProxyGroup {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub proxies: Vec<String>,
    /// 健康检查地址（`url-test` / `fallback` 使用；仅支持 `http://`）。
    #[serde(default)]
    pub url: Option<String>,
    /// 健康检查间隔（秒），缺省 300。
    #[serde(default)]
    pub interval: Option<u64>,
    /// `url-test` 的容差（毫秒）：新节点需优于当前最优该值才切换，避免抖动。
    #[serde(default)]
    pub tolerance: Option<u64>,
}

/// 健康检查任务规格（由 profile 派生，交给 `health` 模块执行）。
#[derive(Debug, Clone)]
pub struct HealthSpec {
    /// 策略组名。
    pub group: String,
    /// 组类型：`url-test` / `fallback`。
    pub kind: String,
    /// 探测地址。
    pub url: String,
    /// 探测间隔。
    pub interval: std::time::Duration,
    /// 容差（毫秒）。
    pub tolerance: u64,
    /// 组成员（已递归展开为真实代理）。
    pub members: Vec<Proxy>,
}

impl Proxy {
    /// WS 请求路径，缺省 `/`。
    #[must_use]
    pub fn ws_path(&self) -> &str {
        match &self.ws_opts {
            Some(opts) if !opts.path.is_empty() => &opts.path,
            _ => "/",
        }
    }

    /// WS Host 头：`ws-opts.headers.Host` 优先，其次 `servername`，最后 `server`。
    #[must_use]
    pub fn ws_host(&self) -> String {
        if let Some(opts) = &self.ws_opts
            && let Some(host) = opts
                .headers
                .get("Host")
                .or_else(|| opts.headers.get("host"))
        {
            return host.clone();
        }
        self.servername
            .clone()
            .unwrap_or_else(|| self.server.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_profile() {
        let json = r#"{
            "proxies": [{
                "name": "hk2", "type": "vless", "server": "hk2.example.com", "port": 443,
                "uuid": "11111111-1111-1111-1111-111111111111", "network": "ws", "tls": true,
                "servername": "hk2.example.com",
                "ws-opts": {"path": "/cdn-api", "headers": {"Host": "hk2.example.com"}}
            }],
            "proxy-groups": [{"name": "proxy", "type": "select", "proxies": ["hk2"]}],
            "rules": ["DOMAIN-SUFFIX,example.com,proxy", "MATCH,proxy"]
        }"#;
        let profile: Profile = serde_json::from_str(json).unwrap();
        assert_eq!(profile.proxies.len(), 1);
        assert_eq!(profile.proxies[0].ws_path(), "/cdn-api");
        assert_eq!(profile.proxies[0].ws_host(), "hk2.example.com");
        assert_eq!(profile.proxy_groups.len(), 1);
        assert_eq!(profile.rules.len(), 2);
    }
}
