//! GEOIP / GEOSITE 数据（纯文本，无 protobuf 依赖）。
//!
//! - GeoIP 文件：每行一个 CIDR，`#` 开头为注释。
//! - GeoSite 文件：v2fly domain-list 文本格式，支持 `domain:` / `full:` /
//!   `keyword:` 前缀与行内 `#` 注释；`include:` 与 `regexp:` 暂不支持（跳过）。
//!
//! 当前只加载 CN 一个分类（用户订阅里最常见的 `GEOIP,CN` / `GEOSITE,cn`）。

use std::{
    net::IpAddr,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use ipnet::IpNet;

/// GeoIP 集合。
#[derive(Debug, Default)]
pub struct GeoIp {
    nets: Vec<IpNet>,
}

impl GeoIp {
    /// 从文件加载。
    ///
    /// # Errors
    ///
    /// 文件读取失败时返回错误；非法行会被跳过。
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("read geoip {}", path.display()))?;
        Ok(Self::parse(&content))
    }

    /// 从文本解析。
    #[must_use]
    pub fn parse(content: &str) -> Self {
        let nets = content
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .filter_map(|line| line.parse::<IpNet>().ok())
            .collect();
        Self { nets }
    }

    /// 判断 IP 是否属于本集合。
    #[must_use]
    pub fn contains(&self, ip: IpAddr) -> bool {
        self.nets.iter().any(|net| cidr_contains(net, ip))
    }

    /// 集合大小（CIDR 条数）。
    #[must_use]
    pub fn len(&self) -> usize {
        self.nets.len()
    }

    /// 是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nets.is_empty()
    }
}

/// GeoSite 单条匹配方式。
#[derive(Debug, Clone, PartialEq, Eq)]
enum SiteRule {
    Suffix(String),
    Full(String),
    Keyword(String),
}

/// GeoSite 集合。
#[derive(Debug, Default)]
pub struct GeoSite {
    rules: Vec<SiteRule>,
}

impl GeoSite {
    /// 从文件加载。
    ///
    /// # Errors
    ///
    /// 文件读取失败时返回错误；不支持的行会被跳过。
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("read geosite {}", path.display()))?;
        Ok(Self::parse(&content))
    }

    /// 从文本解析。
    #[must_use]
    pub fn parse(content: &str) -> Self {
        let mut rules = Vec::new();
        for raw in content.lines() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() || line.starts_with("include:") || line.starts_with("regexp:") {
                continue;
            }
            if let Some(rest) = line.strip_prefix("full:") {
                rules.push(SiteRule::Full(rest.trim().to_ascii_lowercase()));
            } else if let Some(rest) = line.strip_prefix("domain:") {
                rules.push(SiteRule::Suffix(rest.trim().to_ascii_lowercase()));
            } else if let Some(rest) = line.strip_prefix("keyword:") {
                rules.push(SiteRule::Keyword(rest.trim().to_ascii_lowercase()));
            } else {
                rules.push(SiteRule::Suffix(line.to_ascii_lowercase()));
            }
        }
        Self { rules }
    }

    /// 判断域名是否属于本集合。
    #[must_use]
    pub fn contains(&self, domain: &str) -> bool {
        let domain = domain.to_ascii_lowercase();
        self.rules.iter().any(|rule| match rule {
            SiteRule::Suffix(suffix) => suffix_matches(&domain, suffix),
            SiteRule::Full(full) => domain == *full,
            SiteRule::Keyword(keyword) => domain.contains(keyword),
        })
    }

    /// 集合大小（规则条数）。
    #[must_use]
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// 是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}

/// GEOIP / GEOSITE 数据容器。
#[derive(Debug, Default)]
pub struct GeoData {
    pub geoip_cn: Option<GeoIp>,
    pub geosite_cn: Option<GeoSite>,
}

impl GeoData {
    /// 按可选的路径加载数据。
    ///
    /// # Errors
    ///
    /// 任一路径读取失败时返回错误。
    pub fn load(geoip: Option<&PathBuf>, geosite: Option<&PathBuf>) -> Result<Self> {
        let mut data = Self::default();
        if let Some(path) = geoip {
            data.geoip_cn = Some(GeoIp::load(path)?);
        }
        if let Some(path) = geosite {
            data.geosite_cn = Some(GeoSite::load(path)?);
        }
        Ok(data)
    }
}

pub(crate) fn cidr_contains(net: &IpNet, ip: IpAddr) -> bool {
    match (net, ip) {
        (IpNet::V4(net), IpAddr::V4(ip)) => net.contains(&ip),
        (IpNet::V6(net), IpAddr::V6(ip)) => net.contains(&ip),
        _ => false,
    }
}

/// 无分配的后缀匹配：`domain == suffix` 或 `domain` 以 `.suffix` 结尾。
pub(crate) fn suffix_matches(domain: &str, suffix: &str) -> bool {
    if domain == suffix {
        return true;
    }
    domain.len() > suffix.len()
        && domain.ends_with(suffix)
        && domain.as_bytes()[domain.len() - suffix.len() - 1] == b'.'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geoip_matches_cidr() {
        let geo = GeoIp::parse("# comment\n10.0.0.0/8\n\n2001:db8::/32\nbad-line\n");
        assert_eq!(geo.len(), 2);
        assert!(geo.contains("10.1.2.3".parse().unwrap()));
        assert!(!geo.contains("11.0.0.1".parse().unwrap()));
        assert!(geo.contains("2001:db8::1".parse().unwrap()));
    }

    #[test]
    fn geosite_matches_suffix_full_keyword() {
        let site = GeoSite::parse(
            "# comment\ndomain:example.com\nfull:api.test\nkeyword:google\ncn\ntld.example # inline\nregexp:^ad\\.\ninclude:other\n",
        );
        assert!(site.contains("www.example.com"));
        assert!(site.contains("example.com"));
        assert!(site.contains("api.test"));
        assert!(!site.contains("www.api.test"));
        assert!(site.contains("www.google.com"));
        assert!(site.contains("www.tld.example"));
        assert!(!site.contains("example.com.evil"));
    }

    #[test]
    fn suffix_matches_is_boundary_safe() {
        assert!(suffix_matches("a.b.com", "b.com"));
        assert!(suffix_matches("b.com", "b.com"));
        assert!(!suffix_matches("notb.com", "b.com"));
        assert!(!suffix_matches("b.com.evil", "b.com"));
    }
}
