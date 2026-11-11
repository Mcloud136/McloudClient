//! 触发一次 panic，用于验证日志的 panic 记录与 `WorkerGuard` flush。
//!
//! 用法：cargo run -p client-core --example panic_probe -- <log-dir>

fn main() -> anyhow::Result<()> {
    let dir = std::env::args().nth(1).map(std::path::PathBuf::from);
    let _guard = client_core::logging::init(dir)?;
    tracing::info!("panic probe armed");
    panic!("panic probe: intentional");
}
