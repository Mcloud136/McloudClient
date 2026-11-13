//! C2 更新回滚（FA 硬前置，规格二）：升级前记录回滚点 → 新 GUI 启动 60s 健康检查
//! → 失败自动回滚（恢复上一版二进制 + 升级前快照 seq）→ 转发探测确认。
//!
//! 状态文件 `update-state.json` 与快照同目录，原子写。回滚二进制来自安装器
//! 备份的 `<安装目录>\previous\`（离线可回滚，不重新下载）。
//!
//! 渲染层仅 Windows（ADR 0005）：非 Windows 允许 dead_code，单测双平台跑。
#![cfg_attr(not(windows), allow(dead_code))]

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// 回滚状态文件名。
pub const UPDATE_STATE_FILE: &str = "update-state.json";
/// 健康检查窗口（秒）：升级后此时长内持续探测，失败则回滚（FA 规格二）。
pub const HEALTH_WINDOW_SECS: u64 = 60;
/// 健康检查采样间隔（秒）。
pub const HEALTH_INTERVAL_SECS: u64 = 5;
/// 判定回滚所需的连续失败次数（避免单次抖动误回滚）。
pub const HEALTH_FAIL_THRESHOLD: u32 = 3;

/// 升级回滚点：升级前的版本与快照序号，以及安装包路径（取证用）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpdateState {
    /// 升级前 GUI 版本。
    pub prev_version: String,
    /// 升级前快照序号（回滚目标）。
    pub prev_snapshot_seq: u64,
    /// 触发本次升级的安装包路径。
    pub installer_path: String,
    /// 升级发起时刻（Unix 秒），用于 60s 窗口判定。
    pub started_at: u64,
    /// 内核安装目录（回滚二进制恢复目标）。GUI 与内核可能不同目录，
    /// 不能用 GUI 自身路径推导，故在记录回滚点时显式保存。
    #[serde(default)]
    pub install_dir: String,
}

/// 状态文件路径。
#[must_use]
pub fn state_path(snapshot_dir: &Path) -> PathBuf {
    snapshot_dir.join(UPDATE_STATE_FILE)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 升级前记录回滚点（原子写）。
///
/// # Errors
///
/// 目录创建或写入失败时返回错误。
pub fn record_update_state(snapshot_dir: &Path, state: &UpdateState) -> Result<()> {
    std::fs::create_dir_all(snapshot_dir).context("update-state dir")?;
    let bytes = serde_json::to_vec_pretty(state).context("update-state serialize")?;
    let path = state_path(snapshot_dir);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &bytes).context("update-state tmp write")?;
    std::fs::rename(&tmp, &path).context("update-state rename")?;
    Ok(())
}

/// 读取回滚点；不存在返回 `None`。
///
/// # Errors
///
/// 文件存在但 JSON 非法时返回错误。
pub fn load_update_state(snapshot_dir: &Path) -> Result<Option<UpdateState>> {
    match std::fs::read(state_path(snapshot_dir)) {
        Ok(bytes) => Ok(Some(
            serde_json::from_slice(&bytes).context("update-state json")?,
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).context("update-state read"),
    }
}

/// 清除回滚点（健康检查通过或回滚完成后）。
///
/// # Errors
///
/// 删除失败（非「不存在」）时返回错误。
pub fn clear_update_state(snapshot_dir: &Path) -> Result<()> {
    match std::fs::remove_file(state_path(snapshot_dir)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("update-state remove"),
    }
}

/// 回滚点是否仍在健康检查窗口内（超窗视为上次升级已完成，不再自动回滚）。
#[must_use]
pub fn in_health_window(state: &UpdateState, now: u64) -> bool {
    now.saturating_sub(state.started_at) < HEALTH_WINDOW_SECS
}

/// 安装器备份的上一版二进制目录。
#[must_use]
pub fn previous_dir(install_dir: &Path) -> PathBuf {
    install_dir.join("previous")
}

/// 上一版三个二进制是否齐备（回滚前提）。
#[must_use]
pub fn previous_binaries_ready(install_dir: &Path) -> bool {
    let dir = previous_dir(install_dir);
    ["client-core.exe", "client-gui.exe", "sidecar.exe"]
        .iter()
        .all(|name| dir.join(name).is_file())
}

/// 从 `previous\` 恢复上一版**服务二进制**（client-core / sidecar）到安装目录。
///
/// 不自替换正在运行的 `client-gui.exe`（Windows 文件占用）：按 FA 兼容矩阵
/// 「内核 v1 ← 控制面 v2 允许」，回滚后新 GUI 控制旧内核合法；上一版 GUI 仍保留
/// 在 `previous\` 供后续手动重装。
///
/// # Errors
///
/// 备份缺失或复制失败时返回错误。
pub fn restore_previous_binaries(install_dir: &Path) -> Result<()> {
    if !previous_binaries_ready(install_dir) {
        anyhow::bail!(
            "previous binaries not found in {}",
            previous_dir(install_dir).display()
        );
    }
    let dir = previous_dir(install_dir);
    for name in ["client-core.exe", "sidecar.exe"] {
        std::fs::copy(dir.join(name), install_dir.join(name))
            .with_context(|| format!("restore {name}"))?;
    }
    Ok(())
}

/// 单次健康探针：经 IPC 请求内核 `status`，可达即健康。
///
/// 复用既有 IPC（不引新协议）；连接或请求失败均视为不健康。
#[cfg(windows)]
pub async fn health_probe(pipe: &str) -> bool {
    use client_sidecar::IpcTransport;
    use client_sidecar::ipc::pipe::PipeClient;
    match PipeClient::connect(pipe).await {
        Ok(client) => {
            let ok = client
                .request(
                    client_sidecar::contract::METHOD_STATUS,
                    serde_json::Value::Null,
                )
                .await
                .is_ok();
            client.close().await;
            ok
        }
        Err(_) => false,
    }
}

/// 升级后健康检查：窗口内每 `HEALTH_INTERVAL_SECS` 采样，连续
/// `HEALTH_FAIL_THRESHOLD` 次失败判定不健康（避免单次抖动误回滚）。
/// 窗口结束仍未达失败阈值视为健康。
#[cfg(windows)]
pub async fn run_health_check(pipe: &str, started_at: u64) -> bool {
    let mut fails = 0u32;
    loop {
        if now_secs().saturating_sub(started_at) >= HEALTH_WINDOW_SECS {
            return true;
        }
        if health_probe(pipe).await {
            fails = 0;
        } else {
            fails += 1;
            if fails >= HEALTH_FAIL_THRESHOLD {
                return false;
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(HEALTH_INTERVAL_SECS)).await;
    }
}

/// 执行回滚：恢复上一版二进制 + 回退快照到升级前 seq + 重启服务。
///
/// 调用方负责清除回滚点状态文件。回滚后由健康检查/转发探测确认（harness 断言）。
///
/// # Errors
///
/// 备份缺失、快照回退或文件恢复失败时返回错误。
#[cfg(windows)]
pub fn perform_rollback(
    install_dir: &Path,
    snapshot_dir: &Path,
    prev_seq: u64,
    service_name: &str,
) -> Result<()> {
    // 先停服务，释放二进制占用。
    let _ = std::process::Command::new("sc.exe")
        .args(["stop", service_name])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    std::thread::sleep(std::time::Duration::from_millis(1500));
    restore_previous_binaries(install_dir)?;
    let store = client_sidecar::snapshot::SnapshotStore::new(snapshot_dir, 5);
    if !store
        .rollback_to(prev_seq)
        .context("snapshot rollback_to")?
    {
        anyhow::bail!("rollback target seq {prev_seq} not found");
    }
    let _ = std::process::Command::new("sc.exe")
        .args(["start", service_name])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("mcloud-rollback-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn sample_state() -> UpdateState {
        UpdateState {
            prev_version: "0.1.0".to_owned(),
            prev_snapshot_seq: 3,
            installer_path: "C:/tmp/setup.exe".to_owned(),
            started_at: 1000,
            install_dir: "C:/app".to_owned(),
        }
    }

    #[test]
    fn record_load_clear_roundtrip() {
        let dir = temp_dir("roundtrip");
        assert!(load_update_state(&dir).expect("load").is_none());
        record_update_state(&dir, &sample_state()).expect("record");
        let loaded = load_update_state(&dir).expect("load").expect("some");
        assert_eq!(loaded, sample_state());
        clear_update_state(&dir).expect("clear");
        assert!(load_update_state(&dir).expect("load").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clear_is_idempotent() {
        let dir = temp_dir("clear-idem");
        clear_update_state(&dir).expect("clear missing");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn health_window_boundary() {
        let state = sample_state();
        assert!(in_health_window(&state, 1000));
        assert!(in_health_window(&state, 1059));
        assert!(!in_health_window(&state, 1060));
        assert!(!in_health_window(&state, 2000));
        // 时钟回拨不触发回滚。
        assert!(in_health_window(&state, 999));
    }

    #[test]
    fn previous_binaries_detection_and_restore() {
        let dir = temp_dir("prev");
        let install = dir.join("app");
        std::fs::create_dir_all(&install).expect("install dir");
        assert!(!previous_binaries_ready(&install));
        let prev = previous_dir(&install);
        std::fs::create_dir_all(&prev).expect("prev dir");
        for name in ["client-core.exe", "client-gui.exe", "sidecar.exe"] {
            std::fs::write(prev.join(name), b"old-binary").expect("write prev");
            std::fs::write(install.join(name), b"bad-binary").expect("write cur");
        }
        assert!(previous_binaries_ready(&install));
        restore_previous_binaries(&install).expect("restore");
        assert_eq!(
            std::fs::read(install.join("client-core.exe")).expect("read"),
            b"old-binary"
        );
        assert_eq!(
            std::fs::read(install.join("sidecar.exe")).expect("read"),
            b"old-binary"
        );
        // 运行中的 GUI 不自替换（Windows 文件占用）：保持当前版本。
        assert_eq!(
            std::fs::read(install.join("client-gui.exe")).expect("read"),
            b"bad-binary"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_fails_without_backup() {
        let dir = temp_dir("no-backup");
        let install = dir.join("app");
        std::fs::create_dir_all(&install).expect("dir");
        assert!(restore_previous_binaries(&install).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_rejects_corrupt_state() {
        let dir = temp_dir("corrupt");
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(state_path(&dir), b"not json").expect("write");
        assert!(load_update_state(&dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
