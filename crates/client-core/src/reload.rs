//! 订阅拉取与配置热重载。
//!
//! 设计原则：
//! - **先构建、后替换**：新配置解析/校验失败时保留现有路由器，绝不因一次
//!   网络抖动或格式错误导致断网；
//! - **内容哈希去重**：订阅内容没变就不重建（大规则集重建约 95 ms）；
//! - **原子替换**：通过 [`AppContext`] 的 `ArcSwap` 无锁切换，读路径无锁；
//! - **支持本地文件与 HTTP(S)**：HTTP 实现位于 `client_sidecar::fetch`，
//!   与 sidecar 共用一份；Rust 侧不引入 YAML 依赖，订阅需为
//!   `script/clash2profile.py` 产出的 JSON profile。

use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result};
use tokio::time::interval;

use crate::{context::AppContext, geo::GeoData, profile::Profile, router::Router};

/// 配置来源。
#[derive(Debug, Clone)]
pub enum Source {
    /// 本地 JSON profile 文件。
    File(PathBuf),
    /// 远程订阅地址（返回 JSON profile）。
    Url(String),
}

/// 定时重载规格。
#[derive(Debug, Clone)]
pub struct ReloadSpec {
    pub source: Source,
    pub interval: Duration,
}

/// 计算内容哈希，用于判断订阅是否发生变化。
#[must_use]
pub fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

/// 读取一次配置内容。
///
/// # Errors
///
/// 本地文件读取失败、URL 非法、网络/TLS 失败或 HTTP 非 200 时返回错误。
pub async fn fetch(source: &Source) -> Result<Vec<u8>> {
    match source {
        Source::File(path) => {
            std::fs::read(path).with_context(|| format!("read profile {}", path.display()))
        }
        Source::Url(url) => client_sidecar::fetch::fetch_url(url).await,
    }
}

/// 由配置字节构建路由器；解析或校验失败时返回错误，不影响现有配置。
///
/// # Errors
///
/// JSON 解析失败或 profile 校验失败时返回错误。
pub fn build_router(bytes: &[u8], geo: Arc<GeoData>) -> Result<Router> {
    let profile: Profile = serde_json::from_slice(bytes)
        .context("parse profile JSON (YAML needs clash2profile.py)")?;
    Router::from_profile(profile, geo)
}

/// 启动定时重载任务。
pub fn spawn(context: Arc<AppContext>, spec: ReloadSpec, geo: Arc<GeoData>, initial_hash: u64) {
    tracing::info!(
        interval_secs = spec.interval.as_secs(),
        source = ?spec.source,
        "profile auto-reload enabled"
    );
    drop(tokio::spawn(async move {
        let mut ticker = interval(spec.interval);
        ticker.tick().await; // 跳过立即触发的第一次
        let mut last_hash = initial_hash;
        loop {
            ticker.tick().await;
            match reload_once(&context, &spec, &geo, last_hash).await {
                Ok(Some(hash)) => last_hash = hash,
                Ok(None) => {}
                Err(error) => {
                    // 关键：失败时保留当前配置，不打断在线连接。
                    tracing::warn!(%error, "profile reload failed, keeping current config");
                }
            }
        }
    }));
}

async fn reload_once(
    context: &Arc<AppContext>,
    spec: &ReloadSpec,
    geo: &Arc<GeoData>,
    last_hash: u64,
) -> Result<Option<u64>> {
    let bytes = fetch(&spec.source).await?;
    let hash = hash_bytes(&bytes);
    if hash == last_hash {
        return Ok(None);
    }
    // 先完整构建（含 rule-set 读取与校验），成功后再替换。
    let router = build_router(&bytes, Arc::clone(geo))?;
    context.replace_router(router);
    Ok(Some(hash))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_changes_with_content() {
        assert_ne!(hash_bytes(b"a"), hash_bytes(b"b"));
        assert_eq!(hash_bytes(b"a"), hash_bytes(b"a"));
    }

    #[test]
    fn rejects_yaml_as_profile() {
        let err = build_router(b"proxies: []\nrules: []\n", Arc::new(GeoData::default()));
        assert!(err.is_err());
    }
}
