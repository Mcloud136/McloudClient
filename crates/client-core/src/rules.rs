//! 规则解析与匹配。
//!
//! 支持：`DOMAIN`、`DOMAIN-SUFFIX`、`DOMAIN-KEYWORD`、`IP-CIDR`、`IP-CIDR6`、
//! `GEOIP`、`GEOSITE`、`MATCH`。
//! IP 类规则只对 IP 目标生效（域名目标保持域名透传，不在本地解析）；
//! GEOIP/GEOSITE 当前只支持 `CN` 分类。

use std::net::IpAddr;

use anyhow::{Context, Result, bail};
use ipnet::IpNet;

use crate::geo::{GeoData, cidr_contains, suffix_matches};

/// 单条规则。
// `RULE-SET` 是 Clash 的术语，变体名保留 `Rule` 前缀以对齐配置语义。
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rule {
    Domain(String),
    DomainSuffix(String),
    DomainKeyword(String),
    IpCidr(IpNet),
    GeoIp(String),
    GeoSite(String),
    /// 规则集引用，构建路由器时会被展开成具体规则。
    RuleSet(String),
    Match,
}

/// 匹配目标。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Domain(String),
    Ip(IpAddr),
}

impl Rule {
    /// 解析一行规则，返回（规则，策略名）。
    ///
    /// # Errors
    ///
    /// 规则类型不支持、字段缺失或 CIDR 非法时返回错误。
    pub fn parse(line: &str) -> Result<(Self, String)> {
        let parts: Vec<&str> = line.split(',').map(str::trim).collect();
        if parts.is_empty() {
            bail!("empty rule");
        }
        match parts[0].to_ascii_uppercase().as_str() {
            "DOMAIN" => {
                let (value, policy) = field(&parts, 1)?;
                Ok((Self::Domain(value.to_ascii_lowercase()), policy))
            }
            "DOMAIN-SUFFIX" => {
                let (value, policy) = field(&parts, 1)?;
                Ok((
                    Self::DomainSuffix(value.trim_start_matches('.').to_ascii_lowercase()),
                    policy,
                ))
            }
            "DOMAIN-KEYWORD" => {
                let (value, policy) = field(&parts, 1)?;
                Ok((Self::DomainKeyword(value.to_ascii_lowercase()), policy))
            }
            "IP-CIDR" | "IP-CIDR6" => {
                let (value, policy) = field(&parts, 1)?;
                let net: IpNet = value
                    .parse()
                    .with_context(|| format!("parse CIDR {value}"))?;
                Ok((Self::IpCidr(net), policy))
            }
            "GEOIP" => {
                let (value, policy) = field(&parts, 1)?;
                Ok((Self::GeoIp(value.to_ascii_lowercase()), policy))
            }
            "GEOSITE" => {
                let (value, policy) = field(&parts, 1)?;
                Ok((Self::GeoSite(value.to_ascii_lowercase()), policy))
            }
            "RULE-SET" => {
                let (value, policy) = field(&parts, 1)?;
                Ok((Self::RuleSet(value), policy))
            }
            "MATCH" => {
                let policy = parts.get(1).context("MATCH requires a policy")?.to_string();
                Ok((Self::Match, policy))
            }
            other => bail!("unsupported rule type {other}"),
        }
    }

    /// 判断目标是否命中本规则。
    #[must_use]
    pub fn matches(&self, target: &Target, geo: &GeoData) -> bool {
        match (self, target) {
            (Self::Domain(domain), Target::Domain(value)) => domain == value,
            (Self::DomainSuffix(suffix), Target::Domain(value)) => suffix_matches(value, suffix),
            (Self::DomainKeyword(keyword), Target::Domain(value)) => value.contains(keyword),
            (Self::IpCidr(net), Target::Ip(ip)) => cidr_contains(net, *ip),
            (Self::GeoIp(category), Target::Ip(ip)) => {
                category == "cn" && geo.geoip_cn.as_ref().is_some_and(|set| set.contains(*ip))
            }
            (Self::GeoSite(category), Target::Domain(value)) => {
                category == "cn"
                    && geo
                        .geosite_cn
                        .as_ref()
                        .is_some_and(|set| set.contains(value))
            }
            (Self::Match, _) => true,
            // `RULE-SET` 在构建路由器时已被展开，运行期不应出现。
            (Self::RuleSet(_), _) => false,
            _ => false,
        }
    }
}

/// 解析规则集文件内容，展开成具体规则。
///
/// 三种 `behavior`：
/// - `domain`：每行一个域名模式（`+.a.com` / `*.a.com` / `.a.com` / `a.com`）；
/// - `ipcidr`：每行一个 CIDR；
/// - `classical`：每行是**不含策略名**的规则（`DOMAIN-SUFFIX,a.com`）。
///
/// # Errors
///
/// `behavior` 不支持、CIDR 非法或 `classical` 条目类型未知时返回错误。
pub fn parse_rule_set(behavior: &str, content: &str) -> Result<Vec<Rule>> {
    let mut rules = Vec::new();
    for raw in content.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        match behavior {
            "domain" => {
                if let Some(rule) = parse_domain_pattern(line) {
                    rules.push(rule);
                }
            }
            "ipcidr" => {
                let net: IpNet = line
                    .parse()
                    .with_context(|| format!("parse rule-set CIDR {line}"))?;
                rules.push(Rule::IpCidr(net));
            }
            "classical" => rules.push(parse_rule_set_entry(line)?),
            other => bail!("unsupported rule-set behavior {other}"),
        }
    }
    Ok(rules)
}

/// Clash 的 `domain` 行为统一按后缀匹配处理（`+.` / `*.` / `.` 前缀等价）。
fn parse_domain_pattern(line: &str) -> Option<Rule> {
    let value = line
        .trim_start_matches("+.")
        .trim_start_matches("*.")
        .trim_start_matches('.')
        .trim();
    if value.is_empty() {
        return None;
    }
    Some(Rule::DomainSuffix(value.to_ascii_lowercase()))
}

/// `classical` 条目不带策略名，补一个占位策略复用 [`Rule::parse`]。
fn parse_rule_set_entry(line: &str) -> Result<Rule> {
    let leading = line
        .split(',')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_uppercase();
    if leading == "MATCH" {
        return Ok(Rule::Match);
    }
    let (rule, _) = Rule::parse(&format!("{line},DIRECT"))?;
    Ok(rule)
}

fn field(parts: &[&str], index: usize) -> Result<(String, String)> {
    let value = parts.get(index).context("rule missing value")?.to_string();
    let policy = parts
        .get(index + 1)
        .context("rule missing policy")?
        .to_string();
    Ok((value, policy))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::{GeoIp, GeoSite};

    fn target(domain: &str) -> Target {
        Target::Domain(domain.to_owned())
    }

    fn empty_geo() -> GeoData {
        GeoData::default()
    }

    #[test]
    fn parses_and_matches_domain_rules() {
        let (rule, policy) = Rule::parse("DOMAIN-SUFFIX,Example.COM,proxy").unwrap();
        assert_eq!(policy, "proxy");
        assert!(rule.matches(&target("www.example.com"), &empty_geo()));
        assert!(rule.matches(&target("example.com"), &empty_geo()));
        assert!(!rule.matches(&target("example.com.evil"), &empty_geo()));

        let (rule, _) = Rule::parse("DOMAIN,api.example.com,proxy").unwrap();
        assert!(rule.matches(&target("api.example.com"), &empty_geo()));
        assert!(!rule.matches(&target("www.api.example.com"), &empty_geo()));

        let (rule, _) = Rule::parse("DOMAIN-KEYWORD,google,proxy").unwrap();
        assert!(rule.matches(&target("www.google.com"), &empty_geo()));
        assert!(!rule.matches(&target("www.bing.com"), &empty_geo()));
    }

    #[test]
    fn matches_ip_cidr() {
        let (rule, _) = Rule::parse("IP-CIDR,10.0.0.0/8,proxy").unwrap();
        assert!(rule.matches(&Target::Ip("10.1.2.3".parse().unwrap()), &empty_geo()));
        assert!(!rule.matches(&Target::Ip("11.0.0.1".parse().unwrap()), &empty_geo()));
        assert!(!rule.matches(&target("example.com"), &empty_geo()));

        let (rule, _) = Rule::parse("IP-CIDR6,2001:db8::/32,proxy").unwrap();
        assert!(rule.matches(&Target::Ip("2001:db8::1".parse().unwrap()), &empty_geo()));
        assert!(!rule.matches(&Target::Ip("2001:db9::1".parse().unwrap()), &empty_geo()));
    }

    #[test]
    fn matches_geoip_and_geosite() {
        let geo = GeoData {
            geoip_cn: Some(GeoIp::parse("10.0.0.0/8\n")),
            geosite_cn: Some(GeoSite::parse("domain:example.cn\n")),
        };
        let (rule, _) = Rule::parse("GEOIP,CN,DIRECT").unwrap();
        assert!(rule.matches(&Target::Ip("10.1.2.3".parse().unwrap()), &geo));
        assert!(!rule.matches(&Target::Ip("11.0.0.1".parse().unwrap()), &geo));

        let (rule, _) = Rule::parse("GEOSITE,cn,DIRECT").unwrap();
        assert!(rule.matches(&target("www.example.cn"), &geo));
        assert!(!rule.matches(&target("www.example.com"), &geo));

        // 未加载数据时一律不命中。
        assert!(!rule.matches(&target("www.example.cn"), &empty_geo()));
    }

    #[test]
    fn match_rule_catches_all() {
        let (rule, policy) = Rule::parse("MATCH,proxy").unwrap();
        assert_eq!(policy, "proxy");
        assert!(rule.matches(&target("anything.example"), &empty_geo()));
        assert!(rule.matches(&Target::Ip("1.2.3.4".parse().unwrap()), &empty_geo()));
    }
}
