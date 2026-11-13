//! 自动更新（FA 硬前置）：manifest 检查 → 下载 → SHA-256 校验 → 静默安装。
//!
//! manifest 格式：`{ "version", "url", "sha256" }`；URL 可配置（默认仓库 raw，
//! 实机回归用 mock 面板端点）。安装器负责停服务/覆盖/起服务/拉起 GUI；
//! 本模块只负责取回并校验安装包，校验失败绝不执行。

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use client_sidecar::fetch::request;
use client_sidecar::update_sign;
use sha2::{Digest, Sha256};

use crate::panel::PanelError;

/// 编译期内嵌的 Ed25519 公钥（hex，32 字节）。换钥=发新版。
const PUBLIC_KEY_HEX: &str = include_str!("update-public-key.hex");

/// 内嵌公钥字节。
fn public_key() -> Result<Vec<u8>, PanelError> {
    update_sign::hex_decode(PUBLIC_KEY_HEX.trim())
        .map_err(|e| PanelError::Network(format!("embedded public key: {e}")))
}

/// 更新源选择（用户可在 GUI 设置）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateSource {
    /// Gitee 优先，失败回退 GitHub。
    #[default]
    Auto,
    /// 仅 Gitee（国内）。
    Gitee,
    /// 仅 GitHub。
    Github,
}

/// Gitee 镜像 manifest（国内优先）。
pub const GITEE_MANIFEST: &str =
    "https://gitee.com/wxbns/mcloud-client/raw/main/docs/update-manifest.json";
/// GitHub 主仓库 manifest。
pub const GITHUB_MANIFEST: &str =
    "https://raw.githubusercontent.com/Mcloud136/McloudClient/main/docs/update-manifest.json";

/// 按更新源返回有序候选 manifest URL 列表。
#[must_use]
pub fn source_urls(source: UpdateSource) -> Vec<&'static str> {
    match source {
        UpdateSource::Auto => vec![GITEE_MANIFEST, GITHUB_MANIFEST],
        UpdateSource::Gitee => vec![GITEE_MANIFEST],
        UpdateSource::Github => vec![GITHUB_MANIFEST],
    }
}

/// 按更新源依次尝试候选渠道；任一渠道返回合法签名 manifest 即采用。
/// 全部失败时返回最后一个错误（硬拒绝，不降级到未签名路径）。
///
/// # Errors
///
/// 所有渠道均网络失败或验签失败时返回错误。
pub async fn check_sources(source: UpdateSource) -> Result<Option<UpdateInfo>, PanelError> {
    let mut last_err = PanelError::Network("no update source configured".to_owned());
    for url in source_urls(source) {
        match check(url).await {
            Ok(info) => return Ok(info),
            Err(e) => {
                tracing::warn!(%url, error = %e, "update source failed, trying next");
                last_err = e;
            }
        }
    }
    Err(last_err)
}

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
    let signature = value["signature"].as_str().unwrap_or_default().to_owned();
    if version.is_empty() || url.is_empty() || sha256.len() != 64 {
        return Err(PanelError::Network("manifest fields invalid".to_owned()));
    }
    // FA 硬前置：无合法签名一律拒绝（硬拒绝，无降级路径）。
    let message = update_sign::signed_message(&version, &url, &sha256);
    update_sign::verify(&public_key()?, &message, &signature)
        .map_err(|e| PanelError::Network(format!("manifest signature: {e}")))?;
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

    #[test]
    fn source_order_gitee_first() {
        use super::{GITEE_MANIFEST, GITHUB_MANIFEST, UpdateSource, source_urls};
        assert_eq!(
            source_urls(UpdateSource::Auto),
            vec![GITEE_MANIFEST, GITHUB_MANIFEST]
        );
        assert_eq!(source_urls(UpdateSource::Gitee), vec![GITEE_MANIFEST]);
        assert_eq!(source_urls(UpdateSource::Github), vec![GITHUB_MANIFEST]);
    }

    #[test]
    fn embedded_public_key_is_valid() {
        let key = super::public_key().expect("embedded key");
        assert_eq!(key.len(), 32);
    }

    #[test]
    fn signature_verify_accepts_good_rejects_tampered() {
        use client_sidecar::update_sign;
        let (priv_key, pub_key) = update_sign::generate_keypair().expect("keygen");
        let msg =
            update_sign::signed_message("9.9.9", "https://example.invalid/s.exe", &"ab".repeat(32));
        let sig = update_sign::hex_encode(&update_sign::sign(&priv_key, &msg).expect("sign"));
        assert!(update_sign::verify(&pub_key, &msg, &sig).is_ok());
        let tampered = update_sign::signed_message(
            "9.9.9",
            "https://example.invalid/evil.exe",
            &"ab".repeat(32),
        );
        assert!(update_sign::verify(&pub_key, &tampered, &sig).is_err());
        assert!(update_sign::verify(&pub_key, &msg, "").is_err());
    }
}
