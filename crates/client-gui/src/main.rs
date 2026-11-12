//! `client-gui`：McloudClient 主界面（ADR 0005：windows-reactor 原生 WinUI）。
//!
//! 进程模型（`docs/c1-ipc-contract.md` §1）：GUI 与控制面合并为同一进程、
//! 持有到内核的唯一 IPC 连接；内核是独立的数据面进程，GUI 退出不影响转发。
//!
//! 账号凭据边界（ADR 0004 L1）：密码 / auth_data / 订阅 URL 只以 `Secret<T>`
//! 在控制面模块内持有；UI 只渲染脱敏 DTO。
//!
//! 非 Windows：编译桩（GUI 仅 Windows，内核/sidecar 跨平台不变）。

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use client_sidecar::logging;

mod state;
mod task;

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod panel;

#[derive(Debug, Parser)]
#[command(name = "client-gui", version, about = "McloudClient 主界面")]
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

    /// 面板站点基址（登录页高级设置可改）。
    #[arg(long, default_value = "")]
    site: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let _log_guard = logging::init_with(args.log_dir.clone(), "gui")?;
    tracing::info!(pipe = %args.pipe, poll_ms = args.poll_ms, "gui starting");

    #[cfg(windows)]
    {
        let state = state::SharedState::new();
        let probe_default = args
            .default_probe
            .unwrap_or_else(|| "127.0.0.1:80".to_owned());
        // reactor 的 LocalSender 非 Send：IPC 任务经 SharedState revision
        // 供数，渲染层 250 ms 轮询桥接，wake 无需动作。
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
        let (commands, task_handle) = task::spawn(args.pipe.clone(), Arc::clone(&state), wake);
        let handles = app::GuiHandles {
            shared: state,
            commands,
            task: task_handle,
            panel: panel::PanelClient::new(args.site.clone()),
            store: Arc::from(client_sidecar::credential::default_store()),
        };
        let config = app::GuiConfig {
            site: args.site,
            probe_default,
        };
        app::run(handles, config)?;
        tracing::info!("gui exited");
    }
    #[cfg(not(windows))]
    {
        let _ = args;
        println!("client-gui 仅支持 Windows（ADR 0005）；内核与 sidecar 跨平台可用。");
    }
    Ok(())
}
