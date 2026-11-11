//! 配置快照存储。
//!
//! 设计要点（ADR 0001 §9.2）：
//! - **原子写**：先写 `.tmp`、`fsync`、再 `rename`；
//! - **current 指针**：单独文件保存序号，同样原子替换；
//! - **保留 N 版**：按序号清理，支持回滚到上一版；
//! - **引导快照**：序号从 0 开始，保证首次启动不空配置。

use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

const CURRENT: &str = "current.txt";
/// 内核读取的固定路径；每次写入/回滚后与 current 保持同步。
const LIVE: &str = "live.json";

pub struct SnapshotStore {
    dir: PathBuf,
    keep: usize,
}

impl SnapshotStore {
    #[must_use]
    pub fn new(dir: impl Into<PathBuf>, keep: usize) -> Self {
        Self {
            dir: dir.into(),
            keep: keep.max(1),
        }
    }

    pub fn ensure_dir(&self) -> io::Result<()> {
        fs::create_dir_all(&self.dir)
    }

    /// 写入引导快照（序号 0），仅在尚无 `current` 时生效。
    pub fn bootstrap(&self, profile: &[u8]) -> io::Result<u64> {
        if self.current()?.is_some() {
            return Ok(self.current()?.map_or(0, |(seq, _)| seq));
        }
        self.ensure_dir()?;
        self.write_seq(0, profile)?;
        self.update_current(0)?;
        self.sync_live(0)?;
        Ok(0)
    }

    /// 写入新快照并切换 `current`；返回新序号。
    pub fn write(&self, profile: &[u8]) -> io::Result<u64> {
        self.ensure_dir()?;
        let seq = self.next_seq()?;
        self.write_seq(seq, profile)?;
        self.update_current(seq)?;
        self.sync_live(seq)?;
        self.cleanup()?;
        Ok(seq)
    }

    /// 内核读取的固定路径。
    #[must_use]
    pub fn live_path(&self) -> PathBuf {
        self.dir.join(LIVE)
    }

    /// 当前快照（序号 + 路径）。
    pub fn current(&self) -> io::Result<Option<(u64, PathBuf)>> {
        let pointer = self.dir.join(CURRENT);
        let text = match fs::read_to_string(&pointer) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let Ok(seq) = text.trim().parse::<u64>() else {
            return Ok(None);
        };
        let path = self.path_for(seq);
        if path.is_file() {
            Ok(Some((seq, path)))
        } else {
            Ok(None)
        }
    }

    /// 回滚到上一版本；没有更早版本时返回 `false`。
    pub fn rollback(&self) -> io::Result<bool> {
        let Some((current, _)) = self.current()? else {
            return Ok(false);
        };
        let mut seqs = self.list_seqs()?;
        seqs.retain(|seq| *seq < current);
        let Some(previous) = seqs.last() else {
            return Ok(false);
        };
        self.update_current(*previous)?;
        self.sync_live(*previous)?;
        Ok(true)
    }

    fn path_for(&self, seq: u64) -> PathBuf {
        self.dir.join(format!("{seq:04}.json"))
    }

    fn write_seq(&self, seq: u64, profile: &[u8]) -> io::Result<()> {
        let target = self.path_for(seq);
        let tmp = self.dir.join(format!("{seq:04}.json.tmp"));
        let mut file = fs::File::create(&tmp)?;
        file.write_all(profile)?;
        // 确保数据落盘后再改名，避免断电后留下半写文件。
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, &target)?;
        Ok(())
    }

    /// 把指定版本实体化到 `live.json`，供内核以固定路径读取。
    fn sync_live(&self, seq: u64) -> io::Result<()> {
        let source = self.path_for(seq);
        let target = self.live_path();
        let tmp = self.dir.join("live.json.tmp");
        fs::copy(&source, &tmp)?;
        // 副本落盘后再替换，避免内核读到半写内容。
        fs::OpenOptions::new().write(true).open(&tmp)?.sync_all()?;
        fs::rename(&tmp, &target)?;
        Ok(())
    }

    fn update_current(&self, seq: u64) -> io::Result<()> {
        let pointer = self.dir.join(CURRENT);
        let tmp = self.dir.join("current.txt.tmp");
        let mut file = fs::File::create(&tmp)?;
        file.write_all(seq.to_string().as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, &pointer)?;
        Ok(())
    }

    fn next_seq(&self) -> io::Result<u64> {
        Ok(self.list_seqs()?.last().map_or(0, |seq| seq + 1))
    }

    fn list_seqs(&self) -> io::Result<Vec<u64>> {
        let mut seqs = Vec::new();
        if !self.dir.is_dir() {
            return Ok(seqs);
        }
        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(stem) = name.strip_suffix(".json")
                && let Ok(seq) = stem.parse::<u64>()
            {
                seqs.push(seq);
            }
        }
        seqs.sort_unstable();
        Ok(seqs)
    }

    fn cleanup(&self) -> io::Result<()> {
        let seqs = self.list_seqs()?;
        if seqs.len() <= self.keep {
            return Ok(());
        }
        let remove = seqs.len() - self.keep;
        for seq in seqs.into_iter().take(remove) {
            let _ = fs::remove_file(self.path_for(seq));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("mcloud-snapshot-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn bootstrap_then_write_increments_sequence() {
        let dir = temp_dir("seq");
        let store = SnapshotStore::new(&dir, 5);
        assert_eq!(store.bootstrap(b"bootstrap").expect("bootstrap"), 0);
        let (seq, path) = store.current().expect("current").expect("some");
        assert_eq!(seq, 0);
        assert_eq!(fs::read(&path).expect("read"), b"bootstrap");
        // 内核读取的固定路径必须与 current 同步。
        assert_eq!(fs::read(store.live_path()).expect("live"), b"bootstrap");

        assert_eq!(store.write(b"v1").expect("write"), 1);
        let (seq, path) = store.current().expect("current").expect("some");
        assert_eq!(seq, 1);
        assert_eq!(fs::read(&path).expect("read"), b"v1");
        assert_eq!(fs::read(store.live_path()).expect("live"), b"v1");

        // 引导快照不覆盖已有 current。
        assert_eq!(store.bootstrap(b"again").expect("bootstrap"), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn keeps_only_configured_versions() {
        let dir = temp_dir("keep");
        let store = SnapshotStore::new(&dir, 3);
        store.bootstrap(b"boot").expect("bootstrap");
        for i in 1..=5 {
            store.write(format!("v{i}").as_bytes()).expect("write");
        }
        let seqs = store.list_seqs().expect("list");
        assert_eq!(seqs.len(), 3);
        assert_eq!(*seqs.last().expect("last"), 5);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rollback_moves_pointer_back() {
        let dir = temp_dir("rollback");
        let store = SnapshotStore::new(&dir, 5);
        store.bootstrap(b"boot").expect("bootstrap");
        store.write(b"v1").expect("write");
        store.write(b"v2").expect("write");

        assert!(store.rollback().expect("rollback"));
        let (seq, path) = store.current().expect("current").expect("some");
        assert_eq!(seq, 1);
        assert_eq!(fs::read(&path).expect("read"), b"v1");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rollback_returns_false_at_oldest() {
        let dir = temp_dir("oldest");
        let store = SnapshotStore::new(&dir, 5);
        store.bootstrap(b"boot").expect("bootstrap");
        assert!(!store.rollback().expect("rollback"));
        let _ = fs::remove_dir_all(&dir);
    }
}
