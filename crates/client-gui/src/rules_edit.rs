//! C2 分流规则编辑：用户规则独立文件 + 内核同源校验 + 合并注入快照。
//!
//! 架构约束（`client-design-plan.md` §FA 前置规格二）：
//! - UI 偏好（用户自定义规则）放**独立文件** `user-rules.json`，不混入订阅 `live.json`；
//! - 合并结果（用户规则前置 + 订阅规则）写入快照，经 IPC `reload` 生效；
//! - 校验复用内核 `Router::from_profile`（单一事实源），geo 用空数据与服务形态一致；
//! - 兼容矩阵：旧内核读含规则的新快照正常、忽略 `user-rules.json`；
//!   新 GUI 读旧快照（无该文件）回退订阅规则。
//!
//! 渲染层仅 Windows（ADR 0005）：非 Windows 允许 dead_code，单测双平台跑。
#![cfg_attr(not(windows), allow(dead_code))]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use client_core::geo::GeoData;
use client_core::profile::Profile;
use client_core::router::Router;
use serde::{Deserialize, Serialize};

/// 用户规则文件名（与快照同目录，独立于订阅 `live.json`）。
pub const USER_RULES_FILE: &str = "user-rules.json";

/// 用户规则文件载荷：仅一个可选 `rules` 数组（新增字段可选/有默认值，旧版忽略）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct UserRules {
    #[serde(default)]
    pub rules: Vec<String>,
}

/// 用户规则文件路径。
#[must_use]
pub fn user_rules_path(snapshot_dir: &Path) -> PathBuf {
    snapshot_dir.join(USER_RULES_FILE)
}

/// 读取用户规则；文件不存在或为空时返回空规则（兼容旧快照）。
///
/// # Errors
///
/// 文件存在但 JSON 非法时返回错误。
pub fn load_user_rules(snapshot_dir: &Path) -> Result<UserRules> {
    let path = user_rules_path(snapshot_dir);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).context("user-rules json"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(UserRules::default()),
        Err(error) => Err(error).context("user-rules read"),
    }
}

/// 原子写用户规则文件（先写临时文件再 rename，避免半截文件）。
///
/// # Errors
///
/// 目录创建或写入失败时返回错误。
pub fn save_user_rules(snapshot_dir: &Path, rules: &[String]) -> Result<()> {
    std::fs::create_dir_all(snapshot_dir).context("user-rules dir")?;
    let payload = UserRules {
        rules: rules.to_vec(),
    };
    let bytes = serde_json::to_vec_pretty(&payload).context("user-rules serialize")?;
    let path = user_rules_path(snapshot_dir);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &bytes).context("user-rules tmp write")?;
    std::fs::rename(&tmp, &path).context("user-rules rename")?;
    Ok(())
}

/// 把文本框内容拆成规则行：去空白、去空行、去 `#` 注释行。
#[must_use]
pub fn parse_rule_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

/// 合并用户规则（前置）与订阅 profile 的规则，返回新的 profile 字节。
///
/// 用户规则优先匹配；订阅原有规则保留在后。`proxies`/`proxy-groups`/`rule-providers`
/// 不变，保证数据面解析不受 UI 版本影响。
///
/// # Errors
///
/// 订阅 profile JSON 非法时返回错误。
pub fn merge_rules(subscription_bytes: &[u8], user_rules: &[String]) -> Result<Vec<u8>> {
    let mut profile: Profile =
        serde_json::from_slice(subscription_bytes).context("subscription profile json")?;
    let mut merged: Vec<String> = user_rules.to_vec();
    merged.extend(profile.rules.iter().cloned());
    profile.rules = merged;
    serde_json::to_vec(&profile).context("merged profile serialize")
}

/// 用内核同一套校验合并后的规则集是否可构建路由器。
///
/// geo 用空数据：与服务形态（binPath 不带 `--geoip/--geosite`）一致，
/// 因此 `GEOIP/GEOSITE` 规则在此会被拒绝（与服务实际行为对齐）。
///
/// # Errors
///
/// 规则解析失败、策略不存在、规则集缺失或 geo 数据不足时返回错误（内核口径）。
pub fn validate_merged(merged_bytes: &[u8]) -> Result<()> {
    let profile: Profile = serde_json::from_slice(merged_bytes).context("merged profile json")?;
    Router::from_profile(profile, Arc::new(GeoData::default()))
        .map(|_| ())
        .context("rule validation")
}

/// 编辑入口：校验用户规则 + 订阅合并结果，通过则持久化用户规则并返回合并字节。
///
/// 不落快照（快照写入由调用方经 `SubscriptionSync` 完成，便于统一序号与 reload）。
///
/// # Errors
///
/// 用户规则非法（含合并后策略不存在等）时返回错误，且不写任何文件。
pub fn prepare_edit(snapshot_dir: &Path, subscription_bytes: &[u8], text: &str) -> Result<Vec<u8>> {
    let user_rules = parse_rule_lines(text);
    let merged = merge_rules(subscription_bytes, &user_rules)?;
    validate_merged(&merged)?;
    save_user_rules(snapshot_dir, &user_rules)?;
    Ok(merged)
}

/// 订阅同步时重并入已保存的用户规则（无用户规则则原样返回）。
///
/// # Errors
///
/// 用户规则文件非法或合并失败时返回错误。
pub fn reapply_user_rules(snapshot_dir: &Path, subscription_bytes: &[u8]) -> Result<Vec<u8>> {
    let user = load_user_rules(snapshot_dir)?;
    if user.rules.is_empty() {
        return Ok(subscription_bytes.to_vec());
    }
    merge_rules(subscription_bytes, &user.rules)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUB: &str = r#"{
        "proxies":[{"name":"n1","type":"vless","server":"203.0.113.9","port":443,"uuid":"11111111-1111-1111-1111-111111111111","network":"ws"}],
        "proxy-groups":[{"name":"g","type":"select","proxies":["n1"]}],
        "rules":["MATCH,g"]
    }"#;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mcloud-rules-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn parse_skips_blank_and_comments() {
        let lines = parse_rule_lines("  DOMAIN,a.com,g \n\n# comment\n MATCH,DIRECT\n");
        assert_eq!(lines, vec!["DOMAIN,a.com,g", "MATCH,DIRECT"]);
    }

    #[test]
    fn merge_prepends_user_rules() {
        let merged = merge_rules(SUB.as_bytes(), &["DOMAIN-SUFFIX,example.com,DIRECT".into()])
            .expect("merge");
        let profile: Profile = serde_json::from_slice(&merged).expect("parse");
        assert_eq!(profile.rules[0], "DOMAIN-SUFFIX,example.com,DIRECT");
        assert_eq!(profile.rules[1], "MATCH,g");
        assert_eq!(profile.proxies.len(), 1);
    }

    #[test]
    fn validate_accepts_good_and_rejects_bad_policy() {
        let good = merge_rules(SUB.as_bytes(), &["DOMAIN,a.com,DIRECT".into()]).expect("merge");
        assert!(validate_merged(&good).is_ok());
        let bad = merge_rules(SUB.as_bytes(), &["DOMAIN,a.com,NOPE".into()]).expect("merge");
        assert!(validate_merged(&bad).is_err());
    }

    #[test]
    fn validate_rejects_geo_without_data() {
        let geo = merge_rules(SUB.as_bytes(), &["GEOIP,CN,DIRECT".into()]).expect("merge");
        assert!(validate_merged(&geo).is_err());
    }

    #[test]
    fn prepare_edit_persists_and_returns_merged() {
        let dir = temp_dir("prepare");
        let merged =
            prepare_edit(&dir, SUB.as_bytes(), "DOMAIN,a.com,DIRECT\nMATCH,g").expect("prepare");
        assert!(validate_merged(&merged).is_ok());
        let saved = load_user_rules(&dir).expect("load");
        assert_eq!(saved.rules, vec!["DOMAIN,a.com,DIRECT", "MATCH,g"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prepare_edit_rejects_invalid_without_writing() {
        let dir = temp_dir("reject");
        let result = prepare_edit(&dir, SUB.as_bytes(), "DOMAIN,a.com,NOPE");
        assert!(result.is_err());
        assert!(!user_rules_path(&dir).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reapply_merges_saved_user_rules() {
        let dir = temp_dir("reapply");
        save_user_rules(&dir, &["DOMAIN,b.com,DIRECT".into()]).expect("save");
        let merged = reapply_user_rules(&dir, SUB.as_bytes()).expect("reapply");
        let profile: Profile = serde_json::from_slice(&merged).expect("parse");
        assert_eq!(profile.rules[0], "DOMAIN,b.com,DIRECT");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_missing_file_returns_empty() {
        let dir = temp_dir("missing");
        let user = load_user_rules(&dir).expect("load");
        assert!(user.rules.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn compatibility_old_kernel_ignores_user_rules_file() {
        // 旧内核只读 live.json（合并后的 profile），不读 user-rules.json：
        // 合并产物本身是合法 profile，旧内核可正常构建路由器。
        let dir = temp_dir("compat-old");
        let merged = prepare_edit(&dir, SUB.as_bytes(), "DOMAIN,c.com,DIRECT").expect("prepare");
        assert!(user_rules_path(&dir).exists());
        let profile: Profile = serde_json::from_slice(&merged).expect("parse");
        assert!(Router::from_profile(profile, Arc::new(GeoData::default())).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn compatibility_new_gui_reads_old_snapshot_without_user_file() {
        // 新 GUI 读旧快照（无 user-rules.json）：reapply 原样返回订阅规则。
        let dir = temp_dir("compat-new");
        let merged = reapply_user_rules(&dir, SUB.as_bytes()).expect("reapply");
        assert_eq!(merged, SUB.as_bytes());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn user_rules_file_tolerates_unknown_fields() {
        // 新增字段可选/有默认值：含未知字段的文件仍可解析（向前兼容）。
        let dir = temp_dir("unknown");
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(
            user_rules_path(&dir),
            br#"{"rules":["MATCH,DIRECT"],"future_field":42}"#,
        )
        .expect("write");
        let user = load_user_rules(&dir).expect("load");
        assert_eq!(user.rules, vec!["MATCH,DIRECT"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
