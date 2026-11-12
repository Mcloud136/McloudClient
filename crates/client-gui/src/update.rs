//! 自动更新（FA 硬前置）：manifest 检查 → 下载 → SHA-256 校验 → 静默安装。
//!
//! manifest 格式：`{ "version", "url", "sha256" }`；URL 可配置（默认仓库 raw，
//! 实机回归用 mock 面板端点）。安装器负责停服务/覆盖/起服务/拉起 GUI；
//! 本模块只负责取回并校验安装包，校验失败绝不执行。

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use client_sidecar::fetch::request;
use sha2::{Digest, Sha256};

use crate::panel::PanelError;

/// 可用更新描述。
#[derive(Debug, Clone, PartialEq)]
pub struct UpdateInfo {
    pub version: String,
    pub url: String,
    pub sha256: String,
}

/// 点分数字版本比较：`a > b`。
#[must_use]
pub fn newer(a: &str, b: &str) -> bool {
    let pa: Vec<u64> = a.split('.').map(|s| s.parse().unwrap_or(0)).collect();
    let pb: Vec<u64> = b.split('.').map(|s| s.parse().unwrap_or(0)).collect();
    for i in 0..pa.len().max(pb.len()) {
        let x = pa.get(i).copied().unwrap_or(0);
        let y = pb.get(i).copied().unwrap_or(0);
        if x != y {
            return x > y;
        }
    }
    false
}

/// 检查 manifest；无更新返回 `None`。
///
/// # Errors
///
/// 网络失败或 manifest 格式非法时返回错误。
pub async fn check(manifest_url: &str) -> Result<Option<UpdateInfo>, PanelError> {
    let response = request("GET", manifest_url, &[], None)
        .await
        .map_err(|e| PanelError::Network(e.to_string()))?;
    if response.status == 404 {
        return Ok(None);
    }
    if response.status != 200 {
        return Err(PanelError::Network(format!(
            "manifest HTTP {}",
            response.status
        )));
    }
    let value: serde_json::Value = serde_json::from_slice(&response.body)
        .map_err(|e| PanelError::Network(format!("manifest json: {e}")))?;
    let version = value["version"].as_str().unwrap_or_default().to_owned();
    let url = value["url"].as_str().unwrap_or_default().to_owned();
    let sha256 = value["sha256"].as_str().unwrap_or_default().to_owned();
    if version.is_empty() || url.is_empty() || sha256.len() != 64 {
        return Err(PanelError::Network("manifest fields invalid".to_owned()));
    }
    if newer(&version, env!("CARGO_PKG_VERSION")) {
        Ok(Some(UpdateInfo {
            version,
            url,
            sha256,
        }))
    } else {
        Ok(None)
    }
}

/// SHA-256 校验（小写 hex）。
///
/// # Errors
///
/// 摘要不匹配时返回错误。
pub fn verify_sha(bytes: &[u8], expected: &str) -> Result<()> {
    let actual = format!("{:x}", Sha256::digest(bytes));
    if actual != expected.to_ascii_lowercase() {
        bail!("sha256 mismatch: expected {expected}, got {actual}");
    }
    Ok(())
}

/// 下载更新包并校验，写入临时目录；返回安装包路径。
///
/// # Errors
///
/// 下载失败或校验失败时返回错误（不留下可执行产物）。
pub async fn download(info: &UpdateInfo) -> Result<PathBuf, PanelError> {
    let response = request("GET", &info.url, &[], None)
        .await
        .map_err(|e| PanelError::Network(e.to_string()))?;
    if response.status != 200 {
        return Err(PanelError::Network(format!(
            "installer HTTP {}",
            response.status
        )));
    }
    verify_sha(&response.body, &info.sha256).map_err(|e| PanelError::Network(e.to_string()))?;
    let dir = std::env::temp_dir().join("mcloud-update");
    std::fs::create_dir_all(&dir).map_err(|e| PanelError::Network(e.to_string()))?;
    let path = dir.join(format!("McloudClient-Setup-{}.exe", info.version));
    std::fs::write(&path, &response.body).map_err(|e| PanelError::Network(e.to_string()))?;
    Ok(path)
}

/// 启动静默安装器（安装器自行停服务/覆盖/起服务/拉起 GUI）。
///
/// # Errors
///
/// 进程 spawn 失败时返回错误。
pub fn launch_installer(path: &std::path::Path) -> Result<()> {
    std::process::Command::new(path)
        .arg("/S")
        .spawn()
        .context("spawn installer")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{newer, verify_sha};

    #[test]
    fn version_compare() {
        assert!(newer("0.2.0", "0.1.0"));
        assert!(newer("1.0.0", "0.9.9"));
        assert!(!newer("0.1.0", "0.1.0"));
        assert!(!newer("0.1.0", "0.2.0"));
        assert!(newer("0.10.0", "0.9.0"));
    }

    #[test]
    fn sha_verify_accepts_and_rejects() {
        let bytes = b"hello";
        let good = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        assert!(verify_sha(bytes, good).is_ok());
        assert!(verify_sha(bytes, &"0".repeat(64)).is_err());
    }
}
