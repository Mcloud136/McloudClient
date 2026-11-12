//! Windows 服务入口（`--service`）。
//!
//! 服务由 SCM 启动、运行在 session 0；控制面（sidecar / UI）在用户会话
//! 通过命名管道连接，ACL 由 `PipeSecurity` 限制为 `SYSTEM` + 当前用户。
//!
//! 生命周期：`sc start` → 报告 Running → 进入数据面主循环 → `sc stop` 触发
//! 控制处理器置位 shutdown → 主循环退出 → 报告 Stopped。

#[cfg(windows)]
mod imp {
    // windows-service 的 `define_windows_service!` 展开包含平台 FFI 样板，
    // 本模块集中豁免 unsafe，业务代码本身仍是 safe Rust。
    #![allow(unsafe_code)]

    use std::ffi::OsString;
    use std::sync::{Mutex, OnceLock};
    use std::time::Duration;

    use anyhow::{Context, Result};
    use tokio::sync::watch;
    use windows_service::service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType,
    };
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
    use windows_service::{define_windows_service, service_dispatcher};

    use crate::{app, config::Config, logging::WorkerGuard};

    /// 服务名；`sc.exe` 与 SCM 使用同一标识。
    pub const SERVICE_NAME: &str = "McloudCore";

    static SHUTDOWN: OnceLock<watch::Sender<bool>> = OnceLock::new();
    /// `main` 已解析好的命令行配置。
    ///
    /// SCM 传给 `ServiceMain` 的 `arguments` 只有服务名，binPath 中的参数
    /// 在进程命令行里，因此不能靠回调参数重建配置。
    static CONFIG: OnceLock<Config> = OnceLock::new();
    /// 日志 guard 由 `main` 移交到服务线程：SCM 见到 Stopped 后可能立即回收
    /// 进程，`main` 栈上的 drop 未必执行，必须在报告 Stopped 前显式 flush。
    static LOG_GUARD: Mutex<Option<WorkerGuard>> = Mutex::new(None);

    define_windows_service!(ffi_service_main, service_main);

    /// 进入服务分发器，阻塞直到服务停止。
    ///
    /// `log_guard` 移交到服务线程持有，退出前显式 flush（见 `LOG_GUARD`）。
    ///
    /// # Errors
    ///
    /// 进程不是由 SCM 启动时返回错误（例如在控制台直接运行 `--service`）。
    pub fn run(config: Config, log_guard: WorkerGuard) -> Result<()> {
        let _ = CONFIG.set(config);
        *LOG_GUARD
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(log_guard);
        service_dispatcher::start(SERVICE_NAME, ffi_service_main)
            .map_err(|error| anyhow::anyhow!("service dispatcher: {error}"))
    }

    fn service_main(arguments: Vec<OsString>) {
        if let Err(error) = run_service(arguments) {
            tracing::error!(%error, "service exited with error");
            drop(
                LOG_GUARD
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take(),
            );
        }
    }

    fn run_service(_arguments: Vec<OsString>) -> Result<()> {
        let status_handle = service_control_handler::register(SERVICE_NAME, handle_control)
            .map_err(|error| anyhow::anyhow!("register control handler: {error}"))?;

        status_handle
            .set_service_status(ServiceStatus {
                service_type: ServiceType::OWN_PROCESS,
                current_state: ServiceState::Running,
                controls_accepted: ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
                exit_code: ServiceExitCode::Win32(0),
                checkpoint: 0,
                wait_hint: Duration::default(),
                process_id: None,
            })
            .map_err(|error| anyhow::anyhow!("report running: {error}"))?;

        let (tx, rx) = watch::channel(false);
        let _ = SHUTDOWN.set(tx);

        let config = CONFIG
            .get()
            .context("service config not initialized before dispatcher")?
            .clone();

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .context("build tokio runtime")?;
        let result = runtime.block_on(app::run(config, rx));
        if let Err(error) = &result {
            tracing::error!(%error, "service exited with error");
        }
        // 先 flush 再报告 Stopped：SCM 见到 Stopped 后可能立即回收进程，
        // 异步缓冲里的日志尾（含上面的错误行）否则会丢失。
        drop(
            LOG_GUARD
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take(),
        );

        let exit_code = if result.is_ok() {
            ServiceExitCode::Win32(0)
        } else {
            ServiceExitCode::ServiceSpecific(1)
        };
        status_handle
            .set_service_status(ServiceStatus {
                service_type: ServiceType::OWN_PROCESS,
                current_state: ServiceState::Stopped,
                controls_accepted: ServiceControlAccept::empty(),
                exit_code,
                checkpoint: 0,
                wait_hint: Duration::default(),
                process_id: None,
            })
            .map_err(|error| anyhow::anyhow!("report stopped: {error}"))?;

        Ok(())
    }

    fn handle_control(control: ServiceControl) -> ServiceControlHandlerResult {
        match control {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                if let Some(tx) = SHUTDOWN.get() {
                    let _ = tx.send(true);
                }
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        }
    }
}

#[cfg(windows)]
pub use imp::{SERVICE_NAME, run};

#[cfg(not(windows))]
pub const SERVICE_NAME: &str = "McloudCore";

#[cfg(not(windows))]
/// 非 Windows 平台的占位实现。
///
/// # Errors
///
/// 始终返回「不支持」错误。
pub fn run(
    _config: crate::config::Config,
    _log_guard: crate::logging::WorkerGuard,
) -> anyhow::Result<()> {
    anyhow::bail!("--service is only supported on Windows")
}
