//! GUI 偏好持久化（FA：更新源选择）。与 live.json 分离，不进内核快照。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::update::UpdateSource;

const PREFS_FILE: &str = "gui-prefs.json";

/// GUI 偏好（当前仅更新源；后续可扩展）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GuiPrefs {
    #[serde(default)]
    pub update_source: UpdateSource,
}

fn prefs_path(dir: &Path) -> PathBuf {
    dir.join(PREFS_FILE)
}

/// 读取偏好；文件缺失或损坏时返回默认（不阻塞启动）。
#[must_use]
pub fn load(dir: &Path) -> GuiPrefs {
    std::fs::read(prefs_path(dir))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// 原子写入偏好（tmp + rename）。
///
/// # Errors
///
/// 序列化或写入失败时返回错误。
pub fn save(dir: &Path, prefs: &GuiPrefs) -> std::io::Result<()> {
    let path = prefs_path(dir);
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(prefs).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, &path)
}

#[cfg(test)]
mod tests {
    use super::{GuiPrefs, UpdateSource, load, save};

    #[test]
    fn roundtrip_and_default() {
        let dir = std::env::temp_dir().join(format!("mcloud-prefs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        assert_eq!(load(&dir).update_source, UpdateSource::Auto);
        let prefs = GuiPrefs {
            update_source: UpdateSource::Github,
        };
        save(&dir, &prefs).expect("save");
        assert_eq!(load(&dir).update_source, UpdateSource::Github);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn corrupt_file_falls_back_to_default() {
        let dir = std::env::temp_dir().join(format!("mcloud-prefs-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("gui-prefs.json"), b"{not json").expect("write");
        assert_eq!(load(&dir).update_source, UpdateSource::Auto);
        std::fs::remove_dir_all(&dir).ok();
    }
}
