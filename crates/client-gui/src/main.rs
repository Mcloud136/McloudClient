//! `client-gui`：McloudClient 只读状态面板（C1 首里程碑）。
//!
//! 进程模型（`docs/c1-ipc-contract.md` §1）：GUI 与控制面合并为同一进程、
//! 持有到内核的唯一 IPC 连接；内核是独立的数据面进程，GUI 退出不影响转发。
//!
//! 渲染：egui + glow（预研 §5 D1 验证对象）；深色主题，设计 token 集中在
//! [`theme`]。事件驱动重绘：后台任务状态变化时 `request_repaint`，另按轮询
//! 周期保底一拍。
//!
//! 字体：默认拉丁字体（无 CJK）；中文 UI 需先做字体决策（系统字体加载的
//! RSS 代价 vs 内嵌子集字体的体积代价），见 `docs/c1-gui-tun-prestudy.md`。

use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use client_sidecar::logging;

mod app;
mod state;
mod task;
mod theme;

use app::PanelApp;
use state::SharedState;

#[derive(Debug, Parser)]
#[command(name = "client-gui", version, about = "McloudClient 只读状态面板")]
struct Args {
    /// 内核的命名管道名。
    #[arg(long)]
    pipe: String,

    /// 日志目录；缺省为 `%ProgramData%\McloudClient\logs`。
    #[arg(long)]
    log_dir: Option<PathBuf>,

    /// 指标轮询间隔（毫秒），下限 200。
    #[arg(long, default_value_t = 1000)]
    poll_ms: u64,

    /// 探测输入框的初始地址（便于自动化回归）。
    #[arg(long)]
    default_probe: Option<String>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let _log_guard = logging::init_with(args.log_dir.clone(), "gui")?;
    tracing::info!(pipe = %args.pipe, poll_ms = args.poll_ms, "gui starting");

    let state = SharedState::new();
    let poll = Duration::from_millis(args.poll_ms.clamp(200, 60_000));
    let probe_default = args
        .default_probe
        .unwrap_or_else(|| "127.0.0.1:80".to_owned());
    let pipe = args.pipe.clone();

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("McloudClient")
            .with_inner_size([460.0, 640.0])
            .with_min_inner_size([380.0, 460.0]),
        ..Default::default()
    };

    let state_for_app = state.clone();
    // eframe::AppCreator 的返回类型（DynError 为 crate 私有别名，这里展开书写）。
    let app_creator = move |cc: &eframe::CreationContext| -> Result<
        Box<dyn eframe::App>,
        Box<dyn std::error::Error + Send + Sync>,
    > {
        // 深色主题（网络工具惯例）；颜色 token 见 theme.rs。
        cc.egui_ctx.set_visuals(eframe::egui::Visuals::dark());
        let (commands, stop) =
            task::spawn(pipe.clone(), state_for_app.clone(), cc.egui_ctx.clone());
        Ok(Box::new(PanelApp::new(
            pipe,
            state_for_app,
            commands,
            stop,
            probe_default,
            poll,
        )))
    };

    eframe::run_native("McloudClient", options, Box::new(app_creator))?;
    tracing::info!("gui exited");
    Ok(())
}
