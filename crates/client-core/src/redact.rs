//! 日志脱敏：`log_tail` / `diagnostics` 下发前替换节点地址、UUID、加密串、订阅链接。
//!
//! 手写字节扫描，不引入 regex 依赖。替换顺序：节点地址字面量 → URL → UUID → 长 token。
//! 宁可过度脱敏（诊断日志可读性让位于凭据边界，契约 §6 第 3 条）。

use crate::router::Router;

/// 从路由器登记节点构造脱敏器（地址取自 `probe_targets`，不含凭据字段）。
#[must_use]
pub fn from_router(router: &Router) -> Redactor {
    let literals: Vec<String> = router
        .probe_targets()
        .into_iter()
        .flat_map(|(_, address)| {
            let host = address
                .rsplit_once(':')
                .map(|(h, _)| h.trim_matches(|c| c == '[' || c == ']').to_owned())
                .unwrap_or_else(|| address.clone());
            [address, host]
        })
        .filter(|s| !s.is_empty())
        .collect();
    Redactor::from_literals(literals)
}

/// 节点地址字面量 + 通用模式（URL / UUID / 长 base64/hex 串）的替换器。
pub struct Redactor {
    literals: Vec<String>,
}

impl Redactor {
    /// 字面量按长度降序替换，避免短前缀先命中留下残尾。
    #[must_use]
    pub fn from_literals(mut literals: Vec<String>) -> Self {
        literals.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        literals.dedup();
        Self { literals }
    }

    #[must_use]
    pub fn redact(&self, line: &str) -> String {
        let mut out = line.to_owned();
        for literal in &self.literals {
            if out.contains(literal.as_str()) {
                out = out.replace(literal.as_str(), "[REDACTED]");
            }
        }
        let out = redact_urls(&out);
        let out = redact_uuids(&out);
        redact_long_tokens(&out)
    }
}

/// `http(s)://` 起到空白/引号止整段替换（订阅链接含 token）。
fn redact_urls(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut skip_until = 0usize;
    for (idx, ch) in line.char_indices() {
        if idx < skip_until {
            continue;
        }
        let tail = &line[idx..];
        let is_url = tail
            .get(..7)
            .is_some_and(|s| s.eq_ignore_ascii_case("http://"))
            || tail
                .get(..8)
                .is_some_and(|s| s.eq_ignore_ascii_case("https://"));
        if is_url {
            skip_until = tail
                .find(|c: char| c.is_whitespace() || c == '"' || c == '\'')
                .map_or(line.len(), |p| idx + p);
            out.push_str("[REDACTED-URL]");
        } else {
            out.push(ch);
        }
    }
    out
}

fn hex_at(bytes: &[u8], start: usize, len: usize) -> bool {
    bytes
        .get(start..start + len)
        .is_some_and(|s| s.iter().all(u8::is_ascii_hexdigit))
}

/// 8-4-4-4-12 的 UUID 整段替换。
fn redact_uuids(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        let is_uuid = i + 36 <= bytes.len()
            && hex_at(bytes, i, 8)
            && bytes[i + 8] == b'-'
            && hex_at(bytes, i + 9, 4)
            && bytes[i + 13] == b'-'
            && hex_at(bytes, i + 14, 4)
            && bytes[i + 18] == b'-'
            && hex_at(bytes, i + 19, 4)
            && bytes[i + 23] == b'-'
            && hex_at(bytes, i + 24, 12)
            && (i == 0 || !bytes[i - 1].is_ascii_hexdigit());
        if is_uuid {
            out.extend_from_slice(b"[REDACTED-UUID]");
            i += 36;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| line.to_owned())
}

/// ≥32 位的 base64/hex 连续串（密钥、加密串、哈希）整段替换。
fn redact_long_tokens(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if is_token_byte(bytes[i]) {
            let start = i;
            while i < bytes.len() && is_token_byte(bytes[i]) {
                i += 1;
            }
            if i - start >= 32 {
                out.extend_from_slice(b"[REDACTED-KEY]");
            } else {
                out.extend_from_slice(&bytes[start..i]);
            }
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| line.to_owned())
}

fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b'_' | b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_uuid_and_long_tokens() {
        let r = Redactor::from_literals(vec![]);
        let line = "uuid=12345678-1234-1234-1234-123456789abc key=ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789ab end";
        let out = r.redact(line);
        assert!(out.contains("[REDACTED-UUID]"), "{out}");
        assert!(out.contains("[REDACTED-KEY]"), "{out}");
        assert!(!out.contains("123456789abc"), "{out}");
        assert!(!out.contains("ABCDEFGHIJKLMNOPQRSTUVWXYZ"), "{out}");
    }

    #[test]
    fn replaces_subscription_url_with_token() {
        let r = Redactor::from_literals(vec![]);
        let out = r.redact("fetch https://panel.example/sub?token=secret-token-1 failed");
        assert!(out.contains("[REDACTED-URL]"), "{out}");
        assert!(!out.contains("secret-token-1"), "{out}");
        assert!(!out.contains("panel.example"), "{out}");
    }

    #[test]
    fn replaces_node_address_literals_longest_first() {
        let r = Redactor::from_literals(vec!["1.2.3.4".to_owned(), "1.2.3.4:443".to_owned()]);
        let out = r.redact("dial 1.2.3.4:443 failed; host 1.2.3.4 logged");
        assert!(!out.contains("1.2.3.4"), "{out}");
        assert_eq!(out.matches("[REDACTED]").count(), 2, "{out}");
    }

    #[test]
    fn replaces_ipv6_literal() {
        let r = Redactor::from_literals(vec![
            "[2001:db8::1]:443".to_owned(),
            "2001:db8::1".to_owned(),
        ]);
        let out = r.redact("dial [2001:db8::1]:443 ok");
        assert!(!out.contains("2001:db8"), "{out}");
    }

    #[test]
    fn keeps_benign_lines_intact() {
        let r = Redactor::from_literals(vec![]);
        let line = "2026-11-12T20:16:37.462924Z  INFO client_core::app: router ready rules=4";
        assert_eq!(r.redact(line), line);
        let pipe = "ipc server listening pipe=\\\\.\\pipe\\mcloud-client";
        assert_eq!(r.redact(pipe), pipe);
    }

    #[test]
    fn uuid_inside_longer_hex_run_not_double_counted() {
        let r = Redactor::from_literals(vec![]);
        let out = r.redact("aa12345678-1234-1234-1234-123456789abczz");
        assert!(!out.contains("12345678-1234"), "{out}");
    }
}
