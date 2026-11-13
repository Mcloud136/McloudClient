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

use clap::Parser;
use client_sidecar::logging;

mod prefs;
mod rollback;
mod rules_edit;
mod state;
mod sync;
mod task;

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod panel;
#[cfg(windows)]
mod proxy;
#[cfg(windows)]
mod shop;
#[cfg(windows)]
mod update;

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

    /// 快照目录；须与内核 --profile 指向的 live.json 同目录。
    #[arg(long)]
    snapshot_dir: Option<PathBuf>,

    /// 系统代理接管目标（内核入站地址）。
    #[arg(long, default_value = "127.0.0.1:1080")]
    proxy_target: String,

    /// 更新 manifest URL（实机回归可指向 mock 面板）。
    #[arg(
        long,
        default_value = "https://raw.githubusercontent.com/Mcloud136/McloudClient/main/docs/update-manifest.json"
    )]
    update_manifest: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let _log_guard = logging::init_with(args.log_dir.clone(), "gui")?;
    tracing::info!(pipe = %args.pipe, poll_ms = args.poll_ms, "gui starting");

    #[cfg(windows)]
    {
        use std::sync::Arc;

        // 安装/更新场景：GUI 拉起后补启动内核服务（服务可能处于停止态）。
        let _ = std::process::Command::new("sc.exe")
            .args(["start", "McloudCore"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        let state = state::SharedState::new();
        let probe_default = args
            .default_probe
            .unwrap_or_else(|| "127.0.0.1:80".to_owned());
        // reactor 的 LocalSender 非 Send：IPC 任务经 SharedState revision
        // 供数，渲染层 250 ms 轮询桥接，wake 无需动作。
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
        let (commands, task_handle) = task::spawn(args.pipe.clone(), Arc::clone(&state), wake);
        let snapshot_dir = args.snapshot_dir.unwrap_or_else(|| {
            let program_data =
                std::env::var("ProgramData").unwrap_or_else(|_| "C:\\ProgramData".to_owned());
            PathBuf::from(program_data)
                .join("McloudClient")
                .join("snapshots")
        });
        let sync = sync::SubscriptionSync::new(snapshot_dir.clone())?;
        // FA 规格二：升级后启动健康检查；窗口内持续失败则自动回滚（恢复上一版二进制 +
        // 升级前快照 seq），回滚后以旧版继续启动。无回滚点或超窗则跳过。
        if let Ok(Some(ustate)) = rollback::load_update_state(&snapshot_dir) {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            if rollback::in_health_window(&ustate, now) {
                tracing::info!(prev_version = %ustate.prev_version, "post-update health check start");
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?;
                let healthy =
                    rt.block_on(rollback::run_health_check(&args.pipe, ustate.started_at));
                if healthy {
                    tracing::info!("post-update health check passed");
                    rollback::clear_update_state(&snapshot_dir)?;
                } else {
                    tracing::warn!("post-update health check failed, rolling back");
                    // 回滚目标是回滚点记录的内核安装目录（GUI 与内核可能不同目录）。
                    let install_dir = if ustate.install_dir.is_empty() {
                        std::env::current_exe()?
                            .parent()
                            .map(std::path::Path::to_path_buf)
                            .unwrap_or_default()
                    } else {
                        PathBuf::from(&ustate.install_dir)
                    };
                    match rollback::perform_rollback(
                        &install_dir,
                        &snapshot_dir,
                        ustate.prev_snapshot_seq,
                        "McloudCore",
                    ) {
                        Ok(()) => {
                            rollback::clear_update_state(&snapshot_dir)?;
                            tracing::info!("rollback completed, continuing with previous version");
                        }
                        Err(error) => {
                            tracing::error!(%error, "rollback failed");
                        }
                    }
                }
            } else {
                // 超窗：上次升级已完成，清除残留回滚点。
                rollback::clear_update_state(&snapshot_dir)?;
            }
        }
        let handles = app::GuiHandles {
            shared: state,
            commands,
            task: task_handle,
            panel: panel::PanelClient::new(args.site.clone()),
            store: Arc::from(client_sidecar::credential::default_store()),
            sync: Arc::new(tokio::sync::Mutex::new(sync)),
        };
        let prefs = prefs::load(&snapshot_dir);
        let config = app::GuiConfig {
            site: args.site,
            probe_default,
            proxy_target: args.proxy_target,
            update_manifest: args.update_manifest,
            update_source: prefs.update_source,
            prefs_dir: snapshot_dir.clone(),
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
