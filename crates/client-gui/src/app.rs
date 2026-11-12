//! 只读状态面板（C1 首里程碑）。
//!
//! 展示：连接状态、内核指标（uptime/规则/连接/观测字节）、事件流、
//! `node_probe` 结果，以及策略组 / 节点测速与负载均衡状态。
//! 所有数据来自后台 IPC 任务写入的 [`SharedState`]，UI 不直接发起 IPC 请求；
//! 探测与切换经命令通道交给后台任务执行。

use std::sync::Arc;
use std::time::Duration;

use client_sidecar::contract::GroupMode;
use eframe::egui::{self, CentralPanel, Grid, Layout, Panel, RichText, ScrollArea, TextEdit};
use tokio::sync::mpsc;

use crate::state::{
    Command, LinkStatus, ProbeEntry, SharedState, State, fmt_age, fmt_bytes, fmt_uptime,
};
use crate::task::TaskHandle;
use crate::theme;

/// 面板应用。
pub struct PanelApp {
    pipe: String,
    state: Arc<SharedState>,
    commands: mpsc::UnboundedSender<Command>,
    stop: TaskHandle,
    probe_input: String,
    poll: Duration,
}

impl PanelApp {
    /// 由 `main` 在 `CreationContext` 中构造（IPC 任务同时启动）。
    pub fn new(
        pipe: String,
        state: Arc<SharedState>,
        commands: mpsc::UnboundedSender<Command>,
        stop: TaskHandle,
        probe_default: String,
        poll: Duration,
    ) -> Self {
        Self {
            pipe,
            state,
            commands,
            stop,
            probe_input: probe_default,
            poll,
        }
    }

    /// 发送探测命令；后台任务不可用时在结果区给出可见错误。
    fn send_probe(&self) {
        let address = self.probe_input.trim().to_owned();
        if address.is_empty() {
            return;
        }
        tracing::info!(address = %address, "gui probe command issued");
        if self.commands.send(Command::probe(address.clone())).is_err() {
            self.state.update(|s| {
                s.push_probe(client_sidecar::contract::NodeProbeResult::unreachable(
                    address,
                    "ipc_task_stopped",
                ))
            });
        }
    }

    fn kernel_section(&self, ui: &mut egui::Ui, snapshot: &State) {
        section_heading(ui, "KERNEL");
        match &snapshot.kernel {
            Some(metrics) => {
                Grid::new("kernel")
                    .num_columns(2)
                    .spacing([24.0, 4.0])
                    .show(ui, |ui| {
                        stat_row(ui, "uptime", fmt_uptime(metrics.uptime_s));
                        stat_row(ui, "config version", metrics.version.to_string());
                        stat_row(ui, "rules", metrics.rules.to_string());
                        stat_row(ui, "fake-ip entries", metrics.fakeip_entries.to_string());
                        stat_row(ui, "source", metrics.source_kind.clone());
                    });
            }
            None => {
                ui.label(
                    RichText::new("waiting for kernel metrics…")
                        .color(theme::DIM)
                        .italics(),
                );
            }
        }
    }

    fn traffic_section(&self, ui: &mut egui::Ui, snapshot: &State) {
        section_heading(ui, "CONNECTIONS & TRAFFIC");
        match &snapshot.kernel {
            Some(metrics) => {
                Grid::new("traffic")
                    .num_columns(2)
                    .spacing([24.0, 4.0])
                    .show(ui, |ui| {
                        stat_row(
                            ui,
                            "connections total",
                            metrics.connections_total.to_string(),
                        );
                        stat_row(
                            ui,
                            "connections active",
                            metrics.connections_active.to_string(),
                        );
                        stat_row(ui, "bytes up (observed)", fmt_bytes(metrics.bytes_up));
                        stat_row(ui, "bytes down (observed)", fmt_bytes(metrics.bytes_down));
                    });
            }
            None => {
                ui.label(RichText::new("no data").color(theme::DIM).italics());
            }
        }
    }

    fn probe_section(&mut self, ui: &mut egui::Ui, snapshot: &State) {
        section_heading(ui, "NODE PROBE");
        ui.horizontal(|ui| {
            let field = TextEdit::singleline(&mut self.probe_input)
                .desired_width(180.0)
                .hint_text("127.0.0.1:80");
            ui.add(field);
            if ui
                .button("Probe  (Ctrl+P)")
                .on_hover_text("TCP connect RTT via kernel; loopback targets only for now")
                .clicked()
            {
                self.send_probe();
            }
        });
        ui.label(
            RichText::new("loopback host:port only — the allowlist opens to configured nodes in a later milestone")
                .color(theme::DIM)
                .small(),
        );

        for probe in snapshot.probes_recent_first() {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(fmt_age(probe.at.elapsed()))
                        .color(theme::DIM)
                        .small(),
                );
                ui.label(probe_result_text(&probe));
            });
        }
    }

    /// 策略组与节点状态。新增区块置于末尾，避免挤压上方既有区块的可见性。
    fn nodes_section(&self, ui: &mut egui::Ui, snapshot: &State) {
        section_heading(ui, "NODES & BALANCE");
        ui.horizontal(|ui| {
            if ui
                .button("Probe all  (Ctrl+T)")
                .on_hover_text("TCP connect RTT for every configured node")
                .clicked()
            {
                let _ = self.commands.send(Command::probe_nodes());
            }
            if let Some(summary) = &snapshot.last_probe {
                ui.label(RichText::new(summary).color(theme::OK).small());
            }
        });

        let Some(nodes) = &snapshot.nodes else {
            ui.label(
                RichText::new("waiting for node snapshot…")
                    .color(theme::DIM)
                    .italics(),
            );
            return;
        };
        if nodes.nodes.is_empty() && nodes.groups.is_empty() {
            ui.label(RichText::new("no nodes configured").color(theme::DIM));
            return;
        }

        for node in &nodes.nodes {
            ui.horizontal(|ui| {
                ui.label(RichText::new(&node.name).strong());
                let health = node
                    .health_ms
                    .map_or("health -".to_owned(), |ms| format!("health {ms} ms"));
                ui.label(RichText::new(health).color(theme::DIM).small());
                let tcp = match (node.tcp_ok, node.tcp_rtt_ms) {
                    (Some(true), Some(ms)) => {
                        RichText::new(format!("tcp {ms:.2} ms")).color(theme::OK)
                    }
                    (Some(false), _) => RichText::new("tcp fail").color(theme::ERR),
                    _ => RichText::new("tcp -").color(theme::DIM),
                };
                ui.label(tcp.small());
            });
        }

        for group in &nodes.groups {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("{}  [{}]", group.name, group.kind))
                        .strong()
                        .color(theme::ACCENT),
                );
                let mode = match group.mode {
                    GroupMode::Auto => "auto",
                    GroupMode::Manual => "manual",
                };
                ui.label(
                    RichText::new(format!("{mode} -> {}", group.selected))
                        .color(theme::DIM)
                        .small(),
                );
                if group.mode == GroupMode::Manual && ui.small_button("Auto").clicked() {
                    let _ = self.commands.send(Command::auto(group.name.clone()));
                }
            });
            for member in &group.members {
                ui.horizontal(|ui| {
                    let marker = if *member == group.selected { ">" } else { " " };
                    ui.label(RichText::new(marker).color(theme::ACCENT));
                    ui.label(member);
                    if ui.small_button("use").clicked() {
                        let _ = self
                            .commands
                            .send(Command::select(group.name.clone(), member.clone()));
                    }
                });
            }
        }
    }

    fn events_section(&self, ui: &mut egui::Ui, snapshot: &State) {
        section_heading(ui, "EVENTS");
        let events = snapshot.events_recent_first();
        if events.is_empty() {
            ui.label(RichText::new("none").color(theme::DIM).italics());
            return;
        }
        for event in events {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("[{}]", fmt_age(event.received.elapsed())))
                        .color(theme::DIM)
                        .small(),
                );
                ui.label(RichText::new(&event.kind).strong().color(theme::ACCENT));
                ui.label(&event.summary);
            });
        }
    }
}

impl eframe::App for PanelApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if ctx.input(|i| i.key_pressed(egui::Key::P) && i.modifiers.ctrl) {
            self.send_probe();
        }
        if ctx.input(|i| i.key_pressed(egui::Key::T) && i.modifiers.ctrl) {
            let _ = self.commands.send(Command::probe_nodes());
        }
        let snapshot = self.state.snapshot();

        Panel::top("header").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.heading("McloudClient");
                ui.label(RichText::new("read-only panel").color(theme::DIM).small());
                ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                    let (label, color) = match snapshot.link.status {
                        LinkStatus::Connected => ("● connected", theme::OK),
                        LinkStatus::Connecting => ("◌ connecting", theme::WARN),
                        LinkStatus::Disconnected => ("✕ disconnected", theme::ERR),
                    };
                    let age = fmt_age(snapshot.link.since.elapsed());
                    ui.label(
                        RichText::new(format!("{label}  {age}"))
                            .color(color)
                            .strong(),
                    );
                });
            });
            ui.add_space(2.0);
        });

        Panel::bottom("footer").show(ui, |ui| {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("pipe {}", self.pipe))
                        .color(theme::DIM)
                        .small(),
                );
            });
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!(
                        "connects={} failures={}",
                        snapshot.link.connects, snapshot.link.failures
                    ))
                    .color(theme::DIM)
                    .small(),
                );
            });
            if let Some(error) = &snapshot.link.last_error {
                ui.label(
                    RichText::new(format!("last error: {error}"))
                        .color(theme::ERR)
                        .small(),
                );
            }
            ui.add_space(4.0);
        });

        CentralPanel::default().show(ui, |ui| {
            ScrollArea::vertical().show(ui, |ui| {
                self.kernel_section(ui, &snapshot);
                self.traffic_section(ui, &snapshot);
                self.probe_section(ui, &snapshot);
                self.events_section(ui, &snapshot);
                self.nodes_section(ui, &snapshot);
            });
        });

        // 事件驱动重绘之外的保底节拍：与轮询周期对齐（uptime 等字段每秒刷新）。
        // 窗口隐藏时 eframe 不跑 egui pass，仅以 logic 回调唤醒，渲染开销为零。
        ctx.request_repaint_after(self.poll);
    }
}

impl Drop for PanelApp {
    fn drop(&mut self) {
        self.stop.stop();
        tracing::info!("gui panel dropped; ipc task stopping");
    }
}

/// 区块标题。
fn section_heading(ui: &mut egui::Ui, title: &str) {
    ui.add_space(theme::SPACING);
    ui.label(RichText::new(title).strong().color(theme::ACCENT));
    ui.add_space(2.0);
}

/// 网格统计行：左标签右值。
fn stat_row(ui: &mut egui::Ui, label: &str, value: String) {
    ui.label(RichText::new(label).color(theme::DIM));
    ui.label(value);
    ui.end_row();
}

/// 探测结果单行文本与配色。
fn probe_result_text(probe: &ProbeEntry) -> RichText {
    if probe.reachable {
        let rtt = probe
            .rtt_ms
            .map_or("?".to_owned(), |ms| format!("{ms:.2} ms"));
        RichText::new(format!("{}  {}", probe.address, rtt)).color(theme::OK)
    } else {
        let code = probe.error_code.as_deref().unwrap_or("unreachable");
        RichText::new(format!("{}  ✕ {code}", probe.address)).color(theme::ERR)
    }
}

/// 供 theme 单测断言使用的颜色集合（占位，防 dead_code 误报）。
#[cfg(test)]
#[allow(dead_code)]
fn theme_colors() -> [eframe::egui::Color32; 5] {
    [
        theme::ACCENT,
        theme::OK,
        theme::WARN,
        theme::ERR,
        theme::DIM,
    ]
}
