//! 内核日志：按大小轮转的文件输出（ADR 0002 §3）。
//!
//! - 路径：`%ProgramData%\McloudClient\logs\core.log`（可用 `--log-dir` 覆盖）；
//! - 滚动：单文件 10 MB，保留 5 个，上限 50 MB；
//! - 写入：`tracing-appender` 非阻塞，`WorkerGuard` 在退出时 flush。
//!
//! 命名从 ADR 的 `core.YYYY-MM-DD.log` 调整为 `core.log` / `core.1.log`：
//! 按大小滚动时序号比日期更直观，且日期命名无法约束单日体积上限。

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::writer::MakeWriterExt;

/// 单文件上限 10 MB。
pub const MAX_BYTES: u64 = 10 * 1024 * 1024;
/// 保留文件数（含当前文件）。
pub const KEEP: usize = 5;

/// 按大小轮转的文件写入器。
pub struct RollingWriter {
    dir: PathBuf,
    stem: String,
    max_bytes: u64,
    keep: usize,
    file: Option<File>,
    size: u64,
}

impl RollingWriter {
    /// 创建写入器；目录不存在时自动创建，已存在的当前文件以追加方式打开。
    ///
    /// # Errors
    ///
    /// 目录创建或文件打开失败时返回 IO 错误。
    pub fn new(
        dir: impl Into<PathBuf>,
        stem: &str,
        max_bytes: u64,
        keep: usize,
    ) -> io::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        let mut writer = Self {
            dir,
            stem: stem.to_owned(),
            max_bytes: max_bytes.max(1),
            keep: keep.max(2),
            file: None,
            size: 0,
        };
        writer.open_current()?;
        Ok(writer)
    }

    fn path(&self, index: usize) -> PathBuf {
        if index == 0 {
            self.dir.join(format!("{}.log", self.stem))
        } else {
            self.dir.join(format!("{}.{index}.log", self.stem))
        }
    }

    fn open_current(&mut self) -> io::Result<()> {
        let path = self.path(0);
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        self.size = file.metadata()?.len();
        self.file = Some(file);
        Ok(())
    }

    /// 关闭当前文件、后移历史文件、新建当前文件。
    fn rotate(&mut self) -> io::Result<()> {
        self.file = None;
        let _ = fs::remove_file(self.path(self.keep - 1));
        for index in (1..self.keep - 1).rev() {
            let from = self.path(index);
            if from.is_file() {
                let _ = fs::rename(from, self.path(index + 1));
            }
        }
        let current = self.path(0);
        if current.is_file() {
            fs::rename(&current, self.path(1))?;
        }
        self.size = 0;
        self.open_current()
    }
}

impl Write for RollingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.file.is_none() {
            self.open_current()?;
        }
        if self.size > 0 && self.size + buf.len() as u64 > self.max_bytes {
            self.rotate()?;
        }
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| io::Error::other("log file is not open"))?;
        file.write_all(buf)?;
        self.size += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.file.as_mut() {
            Some(file) => file.flush(),
            None => Ok(()),
        }
    }
}

/// 默认日志目录：`%ProgramData%\McloudClient\logs`，非 Windows 环境回退到 `./logs`。
#[must_use]
pub fn default_dir() -> PathBuf {
    std::env::var_os("ProgramData").map_or_else(
        || PathBuf::from("logs"),
        |root| PathBuf::from(root).join("McloudClient").join("logs"),
    )
}

/// 初始化全局订阅器：控制台 + 文件双写。
///
/// 返回值是 `WorkerGuard`，必须在进程存活期间持有，否则后台写入线程会提前退出。
///
/// # Errors
///
/// 日志目录不可创建，或全局订阅器已被初始化时返回错误。
pub fn init(log_dir: Option<PathBuf>) -> Result<WorkerGuard> {
    let dir = log_dir.unwrap_or_else(default_dir);
    let writer = RollingWriter::new(&dir, "core", MAX_BYTES, KEEP)
        .with_context(|| format!("open log dir {}", dir.display()))?;
    let (non_blocking, guard) = tracing_appender::non_blocking(writer);

    // 默认 info：服务模式下 stdout 不可见，若沿用 EnvFilter 的 ERROR 默认值，
    // 启动与停止等关键事件会全部丢失。
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stdout.and(non_blocking))
        .with_ansi(false)
        .try_init()
        .map_err(|error| anyhow::anyhow!("install tracing subscriber: {error}"))?;

    install_panic_hook();
    tracing::info!(dir = %dir.display(), max_bytes = MAX_BYTES, keep = KEEP, "file logging ready");
    Ok(guard)
}

/// panic 先写日志再交给默认 hook：服务模式下 stderr 不可见，否则崩溃无迹可查。
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!(panic = %info, "thread panicked");
        default_hook(info);
    }));
}

/// 日志文件是否落在给定目录下（用于测试与服务启动自检）。
#[must_use]
pub fn current_path(dir: &Path, stem: &str) -> PathBuf {
    dir.join(format!("{stem}.log"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mcloud-log-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn rotates_at_threshold_and_keeps_files() {
        let dir = temp_dir("rotate");
        let mut writer = RollingWriter::new(&dir, "core", 64, 3).expect("writer");

        for _ in 0..20 {
            writer.write_all(b"0123456789").expect("write");
        }
        writer.flush().expect("flush");

        let mut files: Vec<_> = fs::read_dir(&dir)
            .expect("read dir")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        files.sort();
        assert_eq!(files.len(), 3, "keep 限制必须生效: {files:?}");

        // 总大小必须受 keep × max_bytes 约束，且最新写入落在当前文件。
        let total: u64 = files
            .iter()
            .map(|name| fs::metadata(dir.join(name)).expect("meta").len())
            .sum();
        assert!(
            total > 0 && total <= 64 * 3,
            "总大小必须受 keep × max_bytes 约束: {total}"
        );
        let latest = fs::read_to_string(dir.join("core.log")).expect("latest");
        assert!(latest.ends_with("0123456789"), "最新写入必须落在当前文件");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn appends_to_existing_file() {
        let dir = temp_dir("append");
        fs::create_dir_all(&dir).expect("dir");
        fs::write(dir.join("core.log"), b"old").expect("seed");

        let mut writer = RollingWriter::new(&dir, "core", 1024, 5).expect("writer");
        writer.write_all(b"new").expect("write");
        writer.flush().expect("flush");
        assert_eq!(fs::read(dir.join("core.log")).expect("read"), b"oldnew");
        let _ = fs::remove_dir_all(&dir);
    }
}
