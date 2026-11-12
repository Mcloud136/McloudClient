//! GUI 共享状态：后台 IPC 任务写入，UI 线程按帧取快照。
//!
//! 纯数据层，不含 egui 依赖，便于单测；渲染层的快照克隆成本
//! （≤ 60 条事件 + 10 条探测）在 1 Hz 刷新下可忽略。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use client_sidecar::contract::{DEFAULT_PROBE_TIMEOUT_MS, Metrics, NodeProbeResult, NodesSnapshot};
use serde_json::Value;

/// 事件环上限（超出丢弃最旧）。
const EVENT_RING: usize = 50;
/// 探测结果环上限。
const PROBE_RING: usize = 10;
/// 事件摘要截断长度。
const SUMMARY_MAX: usize = 80;

/// UI 发往后台 IPC 任务的命令。
///
/// 字段仅在 Windows 任务实现中被读取；Linux 构建下任务为桩，字段未读。
#[derive(Debug, Clone)]
pub enum Command {
    /// 对指定目标执行 `node_probe`。
    Probe {
        /// `host:port`，当前仅回环目标会被内核放行。
        #[cfg_attr(not(windows), allow(dead_code))]
        address: String,
        /// 探测超时（毫秒）；受 IPC 请求超时 3 s 约束，取契约默认值。
        #[cfg_attr(not(windows), allow(dead_code))]
        timeout_ms: u64,
    },
    /// 对所有已登记节点执行 `probe_nodes`。
    ProbeNodes {
        #[cfg_attr(not(windows), allow(dead_code))]
        timeout_ms: u64,
    },
    /// 锁定策略组成员；`node = None` 表示恢复自动策略。
    SelectNode {
        #[cfg_attr(not(windows), allow(dead_code))]
        group: String,
        #[cfg_attr(not(windows), allow(dead_code))]
        node: Option<String>,
    },
}

impl Command {
    /// 以契约默认超时构造探测命令。
    #[must_use]
    pub fn probe(address: impl Into<String>) -> Self {
        Self::Probe {
            address: address.into(),
            timeout_ms: DEFAULT_PROBE_TIMEOUT_MS,
        }
    }

    /// 构造全节点测速命令。
    #[must_use]
    pub fn probe_nodes() -> Self {
        Self::ProbeNodes {
            timeout_ms: DEFAULT_PROBE_TIMEOUT_MS,
        }
    }

    /// 构造手动锁定命令。
    #[must_use]
    pub fn select(group: impl Into<String>, node: impl Into<String>) -> Self {
        Self::SelectNode {
            group: group.into(),
            node: Some(node.into()),
        }
    }

    /// 构造恢复自动策略命令。
    #[must_use]
    pub fn auto(group: impl Into<String>) -> Self {
        Self::SelectNode {
            group: group.into(),
            node: None,
        }
    }
}

/// 与内核 IPC 的连接状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkStatus {
    /// 尚未建立连接，退避重试中。
    Connecting,
    /// 已连接。仅在 Windows 任务实现中构造。
    #[cfg_attr(not(windows), allow(dead_code))]
    Connected,
    /// 连接建立后中断。
    Disconnected,
}

/// 连接层信息（头部状态与页脚统计的数据源）。
#[derive(Debug, Clone)]
pub struct LinkInfo {
    pub status: LinkStatus,
    /// 状态切换时刻，用于显示「已连接 12s」。
    pub since: Instant,
    /// 累计成功连接次数。
    pub connects: u64,
    /// 累计连接/重试失败次数。
    pub failures: u64,
    /// 最近一次错误（不含订阅地址与 token）。
    pub last_error: Option<String>,
}

impl Default for LinkInfo {
    fn default() -> Self {
        Self {
            status: LinkStatus::Connecting,
            since: Instant::now(),
            connects: 0,
            failures: 0,
            last_error: None,
        }
    }
}

/// 内核推送事件在 GUI 侧的记录。
#[derive(Debug, Clone)]
pub struct GuiEvent {
    pub kind: String,
    /// 单行摘要（如 `applied v2`）。
    pub summary: String,
    pub received: Instant,
}

/// 一次 `node_probe` 的结果记录。
#[derive(Debug, Clone)]
pub struct ProbeEntry {
    pub address: String,
    pub reachable: bool,
    pub rtt_ms: Option<f64>,
    pub error_code: Option<String>,
    pub at: Instant,
}

impl From<NodeProbeResult> for ProbeEntry {
    fn from(result: NodeProbeResult) -> Self {
        Self {
            address: result.address,
            reachable: result.reachable,
            rtt_ms: result.rtt_ms,
            error_code: result.error_code,
            at: Instant::now(),
        }
    }
}

/// 可克隆的状态快照。
#[derive(Debug, Clone, Default)]
pub struct State {
    pub link: LinkInfo,
    pub kernel: Option<Metrics>,
    /// 策略组与节点的可观测快照（1 Hz 轮询）。
    pub nodes: Option<NodesSnapshot>,
    /// 最近一次全节点测速摘要，如 `3/3 reachable`。
    pub last_probe: Option<String>,
    events: VecDeque<GuiEvent>,
    probes: VecDeque<ProbeEntry>,
}

impl State {
    /// 记录一次成功连接。
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn set_connected(&mut self) {
        self.link.connects += 1;
        self.link.status = LinkStatus::Connected;
        self.link.since = Instant::now();
        self.link.last_error = None;
    }

    /// 记录一次连接尝试失败（仍在退避重试）。
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn set_connect_failed(&mut self, error: String) {
        self.link.failures += 1;
        self.link.status = LinkStatus::Connecting;
        self.link.since = Instant::now();
        self.link.last_error = Some(error);
    }

    /// 记录连接中断。
    pub fn set_disconnected(&mut self, error: String) {
        self.link.status = LinkStatus::Disconnected;
        self.link.since = Instant::now();
        self.link.last_error = Some(error);
    }

    /// 应用一轮内核 `metrics` 轮询结果。
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn apply_metrics(&mut self, metrics: Metrics) {
        self.kernel = Some(metrics);
    }

    /// 应用一轮 `nodes` 轮询结果。
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn apply_nodes(&mut self, nodes: NodesSnapshot) {
        self.nodes = Some(nodes);
    }

    /// 记录一次全节点测速摘要。
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn apply_probe_summary(&mut self, summary: String) {
        self.last_probe = Some(summary);
    }

    /// 应用内核推送的事件（如 reload 的 applied/rejected）。
    pub fn apply_event(&mut self, kind: &str, payload: &Value) {
        let summary = event_summary(kind, payload);
        self.events.push_back(GuiEvent {
            kind: kind.to_owned(),
            summary,
            received: Instant::now(),
        });
        while self.events.len() > EVENT_RING {
            self.events.pop_front();
        }
    }

    /// 追加一条探测结果（含本地无法发出请求时的合成错误）。
    pub fn push_probe(&mut self, result: NodeProbeResult) {
        self.probes.push_back(ProbeEntry::from(result));
        while self.probes.len() > PROBE_RING {
            self.probes.pop_front();
        }
    }

    /// 事件按新→旧排列。
    #[must_use]
    pub fn events_recent_first(&self) -> Vec<GuiEvent> {
        self.events.iter().rev().cloned().collect()
    }

    /// 探测结果按新→旧排列。
    #[must_use]
    pub fn probes_recent_first(&self) -> Vec<ProbeEntry> {
        self.probes.iter().rev().cloned().collect()
    }
}

/// 后台任务与 UI 共享的句柄；临界区只做克隆级操作。
#[derive(Debug, Default)]
pub struct SharedState {
    inner: Mutex<State>,
}

impl SharedState {
    /// 创建共享句柄。
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// 后台任务侧更新入口。
    pub fn update(&self, apply: impl FnOnce(&mut State)) {
        if let Ok(mut guard) = self.inner.lock() {
            apply(&mut guard);
        }
    }

    /// UI 侧快照。
    #[must_use]
    pub fn snapshot(&self) -> State {
        self.inner
            .lock()
            .map_or_else(|_| State::default(), |guard| guard.clone())
    }
}

/// 事件摘要：reload 取状态与版本，其余取压缩后的 JSON。
fn event_summary(kind: &str, payload: &Value) -> String {
    if kind == "reload" {
        let state = payload
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        return match payload.get("version").and_then(Value::as_u64) {
            Some(version) => format!("{state} v{version}"),
            None => state.to_owned(),
        };
    }
    truncate(payload.to_string(), SUMMARY_MAX)
}

/// 按字符边界截断并加省略号。
fn truncate(text: String, max: usize) -> String {
    if text.chars().count() <= max {
        return text;
    }
    let cut: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}…")
}

/// 人可读的字节数（观测值展示用）。
#[must_use]
pub fn fmt_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

/// 人可读的 uptime（内核秒数）。
#[must_use]
pub fn fmt_uptime(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3_599 => format!("{}m {:02}s", secs / 60, secs % 60),
        3_600..=86_399 => format!("{}h {:02}m", secs / 3_600, (secs % 3_600) / 60),
        _ => format!("{}d {}h", secs / 86_400, (secs % 86_400) / 3_600),
    }
}

/// 人可读的相对时长（事件/探测的「多久以前」）。
#[must_use]
pub fn fmt_age(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3_599 => format!("{}m", secs / 60),
        _ => format!("{}h", secs / 3_600),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn state_with_link() -> State {
        State::default()
    }

    #[test]
    fn fmt_bytes_covers_unit_boundaries() {
        assert_eq!(fmt_bytes(0), "0 B");
        assert_eq!(fmt_bytes(512), "512 B");
        assert_eq!(fmt_bytes(1024), "1.00 KiB");
        assert_eq!(fmt_bytes(1_048_576), "1.00 MiB");
        assert_eq!(fmt_bytes(3 * 1_048_576), "3.00 MiB");
        assert_eq!(fmt_bytes(5 * 1_073_741_824), "5.00 GiB");
    }

    #[test]
    fn fmt_uptime_covers_ranges() {
        assert_eq!(fmt_uptime(42), "42s");
        assert_eq!(fmt_uptime(59), "59s");
        assert_eq!(fmt_uptime(60), "1m 00s");
        assert_eq!(fmt_uptime(3_723), "1h 02m");
        assert_eq!(fmt_uptime(86_400 * 2 + 3_600), "2d 1h");
    }

    #[test]
    fn fmt_age_covers_ranges() {
        assert_eq!(fmt_age(Duration::from_secs(3)), "3s");
        assert_eq!(fmt_age(Duration::from_secs(125)), "2m");
        assert_eq!(fmt_age(Duration::from_secs(7_200)), "2h");
    }

    #[test]
    fn link_transitions_track_counts_and_errors() {
        let mut state = state_with_link();
        assert_eq!(state.link.status, LinkStatus::Connecting);

        state.set_connect_failed("pipe busy".into());
        assert_eq!(state.link.status, LinkStatus::Connecting);
        assert_eq!(state.link.failures, 1);
        assert_eq!(state.link.last_error.as_deref(), Some("pipe busy"));

        state.set_connected();
        assert_eq!(state.link.status, LinkStatus::Connected);
        assert_eq!(state.link.connects, 1);
        assert_eq!(state.link.last_error, None);

        state.set_disconnected("eof".into());
        assert_eq!(state.link.status, LinkStatus::Disconnected);
        assert_eq!(state.link.last_error.as_deref(), Some("eof"));
    }

    #[test]
    fn metrics_applied_and_snapshot_visible() {
        let shared = SharedState::new();
        let metrics = Metrics {
            uptime_s: 12,
            rules: 5,
            version: 2,
            fakeip_entries: 3,
            connections_total: 7,
            connections_active: 1,
            bytes_up: 128,
            bytes_down: 1_048_576,
            source_kind: "file".to_owned(),
        };
        shared.update(|state| state.apply_metrics(metrics.clone()));
        assert_eq!(shared.snapshot().kernel, Some(metrics));
    }

    #[test]
    fn event_ring_is_bounded_and_newest_wins() {
        let mut state = State::default();
        for i in 0..(EVENT_RING + 10) {
            state.apply_event("reload", &json!({ "state": "applied", "version": i }));
        }
        assert_eq!(state.events.len(), EVENT_RING);
        let newest = state
            .events_recent_first()
            .first()
            .cloned()
            .expect("newest");
        assert_eq!(newest.summary, format!("applied v{}", EVENT_RING + 9));
        assert_eq!(newest.kind, "reload");
    }

    #[test]
    fn unknown_event_summary_is_truncated_payload() {
        let mut state = State::default();
        let payload = json!({ "note": "x".repeat(200) });
        state.apply_event("diag", &payload);
        let summary = state.events_recent_first()[0].summary.clone();
        assert!(summary.chars().count() <= 80);
        assert!(summary.ends_with('…'));
    }

    #[test]
    fn probe_ring_is_bounded_and_mapped_from_contract() {
        let mut state = State::default();
        for _ in 0..(PROBE_RING + 5) {
            state.push_probe(NodeProbeResult {
                address: "127.0.0.1:80".into(),
                reachable: true,
                rtt_ms: Some(1.25),
                error_code: None,
            });
        }
        assert_eq!(state.probes.len(), PROBE_RING);

        state.push_probe(NodeProbeResult::unreachable(
            "127.0.0.1:9".into(),
            "timeout",
        ));
        let newest = state.probes_recent_first()[0].clone();
        assert!(!newest.reachable);
        assert_eq!(newest.error_code.as_deref(), Some("timeout"));
        assert_eq!(newest.rtt_ms, None);
    }

    #[test]
    fn command_probe_uses_contract_default_timeout() {
        let Command::Probe {
            address,
            timeout_ms,
        } = Command::probe("localhost:80")
        else {
            panic!("expected Probe");
        };
        assert_eq!(address, "localhost:80");
        assert_eq!(timeout_ms, DEFAULT_PROBE_TIMEOUT_MS);
    }

    #[test]
    fn node_commands_carry_defaults_and_auto_marker() {
        let Command::ProbeNodes { timeout_ms } = Command::probe_nodes() else {
            panic!("expected ProbeNodes");
        };
        assert_eq!(timeout_ms, DEFAULT_PROBE_TIMEOUT_MS);

        let Command::SelectNode { group, node } = Command::select("auto", "node-b") else {
            panic!("expected SelectNode");
        };
        assert_eq!(group, "auto");
        assert_eq!(node.as_deref(), Some("node-b"));

        let Command::SelectNode { node, .. } = Command::auto("auto") else {
            panic!("expected SelectNode");
        };
        assert_eq!(node, None);
    }

    #[test]
    fn nodes_snapshot_and_probe_summary_applied() {
        let shared = SharedState::new();
        let snapshot = NodesSnapshot {
            groups: vec![client_sidecar::contract::NodeGroupView {
                name: "auto".into(),
                kind: "url-test".into(),
                mode: client_sidecar::contract::GroupMode::Auto,
                selected: "node-a".into(),
                members: vec!["node-a".into(), "node-b".into()],
            }],
            nodes: vec![client_sidecar::contract::NodeView {
                name: "node-a".into(),
                tcp_ok: Some(true),
                tcp_rtt_ms: Some(0.42),
                ..Default::default()
            }],
        };
        shared.update(|state| {
            state.apply_nodes(snapshot.clone());
            state.apply_probe_summary("2/2 reachable".into());
        });
        let view = shared.snapshot();
        assert_eq!(view.nodes, Some(snapshot));
        assert_eq!(view.last_probe.as_deref(), Some("2/2 reachable"));
    }
}
