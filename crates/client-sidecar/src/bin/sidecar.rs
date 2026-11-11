//! sidecar：控制面最小实现。
//!
//! 职责：
//! 1. 维护快照（引导快照 + 原子写 + 保留 5 版）；
//! 2. 启动内核并连接其 IPC 管道；
//! 3. 源文件变化时导入新快照并触发 reload，失败按三态分类报告；
//! 4. 退出时终止内核。
//!
//! S3 范围：本地文件源；订阅 URL、完整 supervisor 退避与 Windows 服务封装留待后续。

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use clap::Parser;
use client_sidecar::ipc::pipe::PipeClient;
use client_sidecar::snapshot::SnapshotStore;
use client_sidecar::{IpcTransport, VERSION};
use serde_json::{Value, json};
use tokio::process::Command;

/// 引导快照：仅放行全部流量直连，保证首次启动不空配置。
const BOOTSTRAP: &str = r#"{"proxies":[],"proxy-groups":[],"rules":["MATCH,DIRECT"]}"#;

#[derive(Debug, Parser)]
#[command(name = "client-sidecar", version, about = "McloudClient 控制面")]
struct Args {
    /// 与内核通信的命名管道名。
    #[arg(long)]
    pipe: String,

    /// 内核可执行文件路径。
    #[arg(long)]
    kernel: PathBuf,

    /// 配置源文件（JSON profile）。
    #[arg(long)]
    source: PathBuf,

    /// 快照目录。
    #[arg(long)]
    snapshot_dir: PathBuf,

    /// 传给内核的 SOCKS5 监听地址。
    #[arg(long, default_value = "127.0.0.1:1080")]
    listen: String,

    /// 源文件轮询间隔（秒）。
    #[arg(long, default_value_t = 2)]
    refresh_secs: u64,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let store = SnapshotStore::new(&args.snapshot_dir, 5);
    store.ensure_dir()?;
    store.bootstrap(BOOTSTRAP.as_bytes())?;

    let mut source_hash = 0u64;
    match std::fs::read(&args.source) {
        Ok(bytes) => match validate(&bytes) {
            Ok(()) => {
                let seq = store.write(&bytes)?;
                source_hash = hash(&bytes);
                println!("imported source as snapshot seq={seq}");
            }
            Err(error) => eprintln!("validate_failed: {error}"),
        },
        Err(error) => eprintln!("fetch_failed: {error}"),
    }

    let (seq, _) = store.current()?.ok_or("no current snapshot")?;
    let live = store.live_path();
    println!(
        "starting kernel with snapshot seq={seq}: {}",
        live.display()
    );

    let mut child = Command::new(&args.kernel)
        .arg("--profile")
        .arg(&live)
        .arg("--listen")
        .arg(&args.listen)
        .arg("--ipc-pipe")
        .arg(&args.pipe)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()?;

    let client = connect_with_retry(&args.pipe).await?;
    let status = client.request("status", json!({"detail": true})).await?;
    println!("kernel status: {status}");

    loop {
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                result?;
                println!("ctrl-c received, shutting down");
                break;
            }
            () = tokio::time::sleep(Duration::from_secs(args.refresh_secs)) => {
                if let Some(status) = child.try_wait()? {
                    eprintln!("kernel exited unexpectedly: {status}");
                    break;
                }
                refresh(&args, &store, &client, &mut source_hash).await;
            }
        }
    }

    client.close().await;
    let _ = child.kill().await;
    Ok(())
}

async fn refresh(args: &Args, store: &SnapshotStore, client: &PipeClient, source_hash: &mut u64) {
    let bytes = match std::fs::read(&args.source) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("fetch_failed: {error}");
            return;
        }
    };
    let new_hash = hash(&bytes);
    if new_hash == *source_hash {
        return;
    }
    if let Err(error) = validate(&bytes) {
        eprintln!("validate_failed: {error}");
        return;
    }
    match store.write(&bytes) {
        Ok(seq) => match client.request("reload", json!({})).await {
            Ok(value) => {
                println!("reload applied seq={seq} {value}");
                *source_hash = new_hash;
            }
            Err(error) => eprintln!("kernel_rejected: {error}"),
        },
        Err(error) => eprintln!("snapshot_write_failed: {error}"),
    }
}

fn validate(bytes: &[u8]) -> Result<(), String> {
    let value: Value = serde_json::from_slice(bytes).map_err(|error| format!("json: {error}"))?;
    let rules = value
        .get("rules")
        .and_then(Value::as_array)
        .ok_or("missing rules")?;
    if rules.is_empty() {
        return Err("rules is empty".to_owned());
    }
    if value.get("proxies").and_then(Value::as_array).is_none() {
        return Err("missing proxies".to_owned());
    }
    Ok(())
}

fn hash(bytes: &[u8]) -> u64 {
    // 与内核侧一致的内容哈希口径：默认 hasher 即可，仅用于变更检测。
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

async fn connect_with_retry(name: &str) -> Result<PipeClient, Box<dyn std::error::Error>> {
    println!("ipc protocol v{VERSION}");
    for attempt in 0..120 {
        match PipeClient::connect(name).await {
            Ok(client) => return Ok(client),
            Err(error) => {
                if attempt % 20 == 0 {
                    eprintln!("waiting for kernel ipc: {error}");
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
    Err(format!("kernel ipc not available: {name}").into())
}
