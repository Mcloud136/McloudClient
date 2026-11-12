//! A2 订阅同步：拉取订阅内容 → 哈希去重 → 校验 → 快照原子写（含 live.json）。
//!
//! 订阅基址可能多值随机轮换（生产盘点 §9），**只能按内容哈希判等**，
//! 不能按 URL 字符串判等。reload 由调用方经 IPC 触发；本模块不碰 IPC。

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use client_sidecar::fetch::fetch_url;
use client_sidecar::secret::Secret;
use client_sidecar::snapshot::SnapshotStore;
use serde_json::Value;

/// 一次同步的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncOutcome {
    /// 内容与上次注入相同，未写快照（多基址轮换下的常态）。
    Unchanged,
    /// 写入新快照，序号为 `seq`；调用方应触发 reload。
    Changed(u64),
}

/// 订阅同步器：持有快照存储与上次注入内容的哈希。
pub struct SubscriptionSync {
    store: SnapshotStore,
    last_hash: Option<u64>,
}

impl SubscriptionSync {
    /// 以快照目录构造；启动时读当前快照内容初始化哈希，避免重启后重复注入。
    ///
    /// # Errors
    ///
    /// 快照目录不可创建时返回错误。
    pub fn new(dir: impl Into<PathBuf>) -> Result<Self> {
        let store = SnapshotStore::new(dir, 5);
        store.ensure_dir().context("snapshot dir")?;
        let last_hash = match store.current() {
            Ok(Some((_, path))) => std::fs::read(&path).ok().map(|bytes| hash_bytes(&bytes)),
            _ => None,
        };
        Ok(Self { store, last_hash })
    }

    /// 拉取订阅 URL 内容并按需注入快照。
    ///
    /// # Errors
    ///
    /// 拉取失败、内容非法或快照写入失败时返回错误；调用方据此降级，
    /// 已生效快照不受影响。
    pub async fn sync_once(&mut self, url: &Secret<String>) -> Result<SyncOutcome> {
        let bytes = fetch_url(url.expose())
            .await
            .context("fetch subscription")?;
        let new_hash = hash_bytes(&bytes);
        if self.last_hash == Some(new_hash) {
            return Ok(SyncOutcome::Unchanged);
        }
        validate(&bytes)?;
        let seq = self.store.write(&bytes).context("snapshot write")?;
        self.last_hash = Some(new_hash);
        Ok(SyncOutcome::Changed(seq))
    }
}

/// 与 sidecar 导入口径一致的最小校验：JSON 且 rules 非空数组。
fn validate(bytes: &[u8]) -> Result<()> {
    let value: Value = serde_json::from_slice(bytes).context("subscription json")?;
    let rules = value
        .get("rules")
        .and_then(Value::as_array)
        .context("subscription missing rules")?;
    if rules.is_empty() {
        bail!("subscription rules empty");
    }
    Ok(())
}

/// FNV-1a 64：稳定、无依赖，仅用于内容判等（非安全用途）。
fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::{SubscriptionSync, SyncOutcome, validate};
    use client_sidecar::secret::Secret;

    #[test]
    fn validate_rejects_empty_and_missing_rules() {
        assert!(validate(br#"{"rules":[]}"#).is_err());
        assert!(validate(br#"{}"#).is_err());
        assert!(validate(b"not json").is_err());
        assert!(validate(br#"{"rules":["MATCH,DIRECT"]}"#).is_ok());
    }

    #[tokio::test]
    async fn dedup_by_content_not_url() {
        let dir = std::env::temp_dir().join(format!("mcloud-sync-test-{}", std::process::id()));
        let mut sync = SubscriptionSync::new(&dir).expect("store");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut socket, _) = listener.accept().await.expect("accept");
                let mut buf = vec![0u8; 4096];
                let _ = tokio::io::AsyncReadExt::read(&mut socket, &mut buf).await;
                let body = br#"{"rules":["MATCH,DIRECT"]}"#;
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = tokio::io::AsyncWriteExt::write_all(&mut socket, head.as_bytes()).await;
                let _ = tokio::io::AsyncWriteExt::write_all(&mut socket, body).await;
            }
        });
        // 同一内容、不同 URL（模拟多基址轮换）：第二次必须 Unchanged。
        let url1 = Secret::new(format!("http://{addr}/sub?token=a"));
        let url2 = Secret::new(format!("http://{addr}/sub?token=b"));
        assert_eq!(
            sync.sync_once(&url1).await.expect("sync1"),
            SyncOutcome::Changed(0)
        );
        assert_eq!(
            sync.sync_once(&url2).await.expect("sync2"),
            SyncOutcome::Unchanged
        );
        server.await.expect("server");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
