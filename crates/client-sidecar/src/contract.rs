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
pub const METHOD_NODES: &str = "nodes";
pub const METHOD_PROBE_NODES: &str = "probe_nodes";
pub const METHOD_SELECT_NODE: &str = "select_node";

/// 本轮已实现并可实机验证的方法。
pub const IMPLEMENTED_METHODS: &[&str] = &[
    METHOD_STATUS,
    METHOD_RELOAD,
    METHOD_METRICS,
    METHOD_NODE_PROBE,
    METHOD_NODES,
    METHOD_PROBE_NODES,
    METHOD_SELECT_NODE,
];

/// 契约已定义、实现留到 C1 后续里程碑的方法。
pub const PLANNED_METHODS: &[&str] = &["log_tail", "diagnostics"];

pub const ERR_UNKNOWN_METHOD: &str = "unknown_method";
pub const ERR_INVALID_PARAMS: &str = "invalid_params";
pub const ERR_FORBIDDEN_TARGET: &str = "forbidden_target";
pub const ERR_UNKNOWN_GROUP: &str = "unknown_group";
pub const ERR_UNKNOWN_NODE: &str = "unknown_node";

/// `select_node` 中恢复自动策略的保留字。
pub const AUTO_NODE: &str = "auto";

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

// ---------------------------------------------------------------------------
// 多节点测速与负载均衡（C1 续作）
// ---------------------------------------------------------------------------

/// 策略组运行模式。
///
/// `Manual` 表示用户经 `select_node` 锁定了成员，优先于任何自动策略；
/// `Auto` 表示按组类型（`url-test` / `fallback` / `load-balance` / `select`）选路。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GroupMode {
    #[default]
    Auto,
    Manual,
}

/// 单个已登记节点的可观测状态。
///
/// **脱敏边界**：只暴露节点名，绝不回显 `server` / `port` / UUID / 加密串 / 订阅链接。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct NodeView {
    pub name: String,
    /// 经代理的健康检查耗时（毫秒）；`None` 为未知或最近一次失败。
    pub health_ms: Option<u32>,
    /// 最近一次 `probe_nodes` 的 TCP 连接耗时（毫秒）。
    pub tcp_rtt_ms: Option<f64>,
    /// 最近一次 TCP 探测是否成功；`None` 为尚未探测。
    pub tcp_ok: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tcp_error: Option<String>,
}

/// 策略组视图。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NodeGroupView {
    pub name: String,
    /// `select` / `url-test` / `fallback` / `load-balance`。
    pub kind: String,
    pub mode: GroupMode,
    /// 当前生效的成员名；`load-balance` 的 `Auto` 模式下是「下一次」预览值。
    pub selected: String,
    /// 组成员名，保持 profile 中的顺序。
    pub members: Vec<String>,
}

/// `nodes` 返回的完整快照：策略组 + 已登记节点状态。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct NodesSnapshot {
    pub groups: Vec<NodeGroupView>,
    pub nodes: Vec<NodeView>,
}

/// `select_node` 请求。
///
/// `node` 为 `None` 或 [`AUTO_NODE`] 时恢复自动策略。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SelectNodeParams {
    pub group: String,
    #[serde(default)]
    pub node: Option<String>,
}

/// `select_node` 响应。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SelectNodeResult {
    pub group: String,
    pub mode: GroupMode,
    pub selected: String,
}

/// `probe_nodes` 请求。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProbeNodesParams {
    #[serde(default = "default_probe_timeout")]
    pub timeout_ms: u64,
}

/// 单个节点的 TCP 探测结果（按节点名标识，不回显地址）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NodeProbeOutcome {
    pub name: String,
    pub reachable: bool,
    pub rtt_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

/// `probe_nodes` 响应。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ProbeNodesResult {
    pub results: Vec<NodeProbeOutcome>,
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
        assert_eq!(IMPLEMENTED_METHODS.len(), 7);
    }

    #[test]
    fn nodes_snapshot_roundtrip_and_redaction() {
        let snapshot = NodesSnapshot {
            groups: vec![NodeGroupView {
                name: "auto".to_owned(),
                kind: "url-test".to_owned(),
                mode: GroupMode::Manual,
                selected: "node-b".to_owned(),
                members: vec!["node-a".to_owned(), "node-b".to_owned()],
            }],
            nodes: vec![NodeView {
                name: "node-a".to_owned(),
                health_ms: Some(120),
                tcp_rtt_ms: Some(0.42),
                tcp_ok: Some(true),
                tcp_error: None,
            }],
        };
        let encoded = serde_json::to_string(&snapshot).expect("encode");
        assert_eq!(
            serde_json::from_str::<NodesSnapshot>(&encoded).expect("decode"),
            snapshot
        );
        // 脱敏：载荷中不得出现任何地址/凭据类字段。
        for forbidden in ["server", "uuid", "port", "token", "encryption", "address"] {
            assert!(!encoded.contains(forbidden), "leaked field: {forbidden}");
        }
        assert!(encoded.contains("\"mode\":\"manual\""));
    }

    #[test]
    fn probe_and_select_params_defaults() {
        let probe: ProbeNodesParams = serde_json::from_str("{}").expect("probe params");
        assert_eq!(probe.timeout_ms, DEFAULT_PROBE_TIMEOUT_MS);

        let select: SelectNodeParams =
            serde_json::from_str(r#"{"group":"auto"}"#).expect("select params");
        assert_eq!(select.group, "auto");
        assert_eq!(select.node, None);
        assert_eq!(AUTO_NODE, "auto");
    }

    #[test]
    fn node_probe_outcome_omits_optional_error() {
        let outcome = NodeProbeOutcome {
            name: "node-a".to_owned(),
            reachable: true,
            rtt_ms: Some(1.5),
            error_code: None,
        };
        let encoded = serde_json::to_string(&outcome).expect("encode");
        assert!(!encoded.contains("error_code"));
        assert!(!encoded.contains("127.0.0.1"));
    }
}
