//! C1 IPC 契约：方法名、载荷与错误码的单一事实源。
//!
//! 内核、sidecar 与未来的 GUI 共用这些定义，避免方法字符串散落在各处。
//! 帧格式与版本协商仍由 [`crate::protocol`] 负责。

use std::net::IpAddr;

use serde::{Deserialize, Serialize};

pub const METHOD_STATUS: &str = "status";
pub const METHOD_RELOAD: &str = "reload";
pub const METHOD_METRICS: &str = "metrics";
pub const METHOD_NODE_PROBE: &str = "node_probe";

/// 本轮已实现并可实机验证的方法。
pub const IMPLEMENTED_METHODS: &[&str] = &[
    METHOD_STATUS,
    METHOD_RELOAD,
    METHOD_METRICS,
    METHOD_NODE_PROBE,
];

/// 契约已定义、实现留到 C1 后续里程碑的方法。
pub const PLANNED_METHODS: &[&str] = &["log_tail", "diagnostics"];

pub const ERR_UNKNOWN_METHOD: &str = "unknown_method";
pub const ERR_INVALID_PARAMS: &str = "invalid_params";
pub const ERR_FORBIDDEN_TARGET: &str = "forbidden_target";

pub const DEFAULT_PROBE_TIMEOUT_MS: u64 = 2_000;
pub const MAX_PROBE_TIMEOUT_MS: u64 = 10_000;

fn default_probe_timeout() -> u64 {
    DEFAULT_PROBE_TIMEOUT_MS
}

/// 内核内部状态快照。
///
/// 进程 RSS / CPU 由控制面在本地采样，不经 IPC；订阅 URL 与 token 不出现在任何字段中。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Metrics {
    pub uptime_s: u64,
    pub rules: usize,
    pub version: u64,
    pub fakeip_entries: usize,
    pub connections_total: u64,
    pub connections_active: u64,
    /// 客户端 → 目标方向的观测字节数（`select!` 收尾时可能少计最后一段）。
    pub bytes_up: u64,
    /// 目标 → 客户端方向的观测字节数。
    pub bytes_down: u64,
    /// `file` 或 `url`，仅类别，绝不回显地址。
    pub source_kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NodeProbeParams {
    /// `host:port`；当前仅允许回环目标。
    pub address: String,
    #[serde(default = "default_probe_timeout")]
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NodeProbeResult {
    pub address: String,
    pub reachable: bool,
    /// 成功时微秒级浮点毫秒值，失败为 `None`。
    pub rtt_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

impl NodeProbeResult {
    #[must_use]
    pub fn unreachable(address: String, code: &str) -> Self {
        Self {
            address,
            reachable: false,
            rtt_ms: None,
            error_code: Some(code.to_owned()),
        }
    }
}

/// 仅允许回环目标，防止 IPC 被用作任意内网探测。
///
/// 要求严格的 `host:port`；host 必须是 `localhost` 或能解析为 loopback 的 IP 字面量，
/// 因此 `127.0.0.1.evil.example` 这类形近域名不会通过。
#[must_use]
pub fn is_loopback_target(address: &str) -> bool {
    let Some((host, port)) = address.rsplit_once(':') else {
        return false;
    };
    if port.parse::<u16>().is_err() {
        return false;
    }
    let host = host.trim_matches(|c| c == '[' || c == ']');
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_targets_accepted() {
        for address in ["127.0.0.1:8080", "127.0.0.2:1", "localhost:80", "[::1]:443"] {
            assert!(is_loopback_target(address), "{address}");
        }
    }

    #[test]
    fn non_loopback_and_spoofed_hosts_rejected() {
        for address in [
            "10.0.0.1:80",
            "172.16.0.1:80",
            "127.0.0.1.evil.example:80",
            "example.com:443",
            "127.0.0.1",
            "127.0.0.1:99999",
        ] {
            assert!(!is_loopback_target(address), "{address}");
        }
    }

    #[test]
    fn params_default_timeout() {
        let params: NodeProbeParams =
            serde_json::from_str(r#"{"address":"127.0.0.1:1"}"#).expect("parse params");
        assert_eq!(params.timeout_ms, DEFAULT_PROBE_TIMEOUT_MS);
        assert_eq!(params.address, "127.0.0.1:1");
    }

    #[test]
    fn metrics_roundtrip() {
        let metrics = Metrics {
            uptime_s: 42,
            rules: 5,
            version: 2,
            fakeip_entries: 3,
            connections_total: 9,
            connections_active: 1,
            bytes_up: 128,
            bytes_down: 1_048_576,
            source_kind: "file".to_owned(),
        };
        let value = serde_json::to_string(&metrics).expect("encode");
        assert_eq!(
            serde_json::from_str::<Metrics>(&value).expect("decode"),
            metrics
        );
    }

    #[test]
    fn implemented_and_planned_methods_do_not_overlap() {
        for method in PLANNED_METHODS {
            assert!(!IMPLEMENTED_METHODS.contains(method));
        }
    }
}
