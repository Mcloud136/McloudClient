//! 规则路由：把目标映射到出站。
//!
//! 内置策略：`DIRECT`（直连）、`REJECT`（拒绝）。
//!
//! 策略组选择：
//! - `select`：取首个成员，可被 `select_node` 手动锁定；
//! - `url-test`：取健康检查延迟最低的成员，全不可用时退化为首个；
//! - `fallback`：取第一个健康成员，全不可用时退化为首个；
//! - `load-balance`：轮询。
//!
//! 手动锁定（`selections`）优先于任何自动策略，并在配置热重载后继承。
//! `RULE-SET` 在构建时**预展开**成具体规则，运行期零间接开销。

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, bail};
use client_sidecar::contract::{AUTO_NODE, GroupMode, NodeGroupView, NodeView, NodesSnapshot};

use crate::{
    geo::GeoData,
    profile::{HealthSpec, Profile, Proxy, ProxyGroup, RuleProvider},
    rules::{Rule, Target, parse_rule_set},
};

/// 路由结果。
#[derive(Debug, Clone)]
pub enum Route {
    Proxy(Box<Proxy>),
    Direct,
    Reject,
}

/// 节点健康状态：`Some(ms)` 表示最近一次探测成功及其耗时。
#[derive(Debug, Default)]
pub struct HealthTable {
    entries: Mutex<HashMap<String, Option<u32>>>,
}

impl HealthTable {
    /// 记录一次探测结果。
    pub fn set(&self, name: &str, latency: Option<u32>) {
        if let Ok(mut guard) = self.entries.lock() {
            guard.insert(name.to_owned(), latency);
        }
    }

    /// 读取某节点最近一次探测耗时；`None` 表示未知或失败。
    #[must_use]
    pub fn get(&self, name: &str) -> Option<u32> {
        self.entries
            .lock()
            .ok()
            .and_then(|guard| guard.get(name).copied().flatten())
    }

    fn snapshot(&self) -> HashMap<String, Option<u32>> {
        self.entries
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }
}

/// `set_selection` 的失败原因（由 IPC 层映射为错误码）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionError {
    UnknownGroup,
    UnknownNode,
    Poisoned,
}

/// 路由器。
pub struct Router {
    proxies: HashMap<String, Proxy>,
    groups: HashMap<String, ProxyGroup>,
    rules: Vec<(Rule, String)>,
    /// 原始规则行（按配置顺序，未展开 RULE-SET），供 `get_rules` 只读下发。
    rule_lines: Vec<String>,
    round_robin: AtomicUsize,
    geo: Arc<GeoData>,
    health: HealthTable,
    /// 用户手动锁定的组选择：组名 → 直接成员名。
    selections: RwLock<HashMap<String, String>>,
}

impl Router {
    /// 由 profile 与 geo 数据构建路由器，并校验规则引用的策略与数据存在。
    ///
    /// `RULE-SET` 会在此处读取本地规则文件并展开。
    ///
    /// # Errors
    ///
    /// 代理类型不支持、规则解析失败、策略不存在、规则集缺失/为空、
    /// GEOIP/GEOSITE 分类不支持或缺少对应数据时返回错误。
    pub fn from_profile(profile: Profile, geo: Arc<GeoData>) -> Result<Self> {
        let Profile {
            proxies: proxy_list,
            proxy_groups: group_list,
            rule_providers,
            rules: rule_lines,
        } = profile;

        for proxy in &proxy_list {
            validate_proxy(proxy)?;
        }
        let proxies: HashMap<String, Proxy> = proxy_list
            .into_iter()
            .map(|proxy| (proxy.name.clone(), proxy))
            .collect();
        let groups: HashMap<String, ProxyGroup> = group_list
            .into_iter()
            .map(|group| (group.name.clone(), group))
            .collect();

        let mut rules = Vec::new();
        for line in &rule_lines {
            let (rule, policy) = Rule::parse(line)?;
            if !proxies.contains_key(&policy)
                && !groups.contains_key(&policy)
                && !is_builtin(&policy)
            {
                bail!("rule policy {policy} not found");
            }
            for expanded in expand_rule(&rule, &rule_providers, line)? {
                validate_geo_rule(&expanded, geo.as_ref(), line)?;
                rules.push((expanded, policy.clone()));
            }
        }
        if rules.is_empty() {
            bail!("profile has no rules");
        }
        Ok(Self {
            proxies,
            groups,
            rules,
            rule_lines,
            round_robin: AtomicUsize::new(0),
            geo,
            health: HealthTable::default(),
            selections: RwLock::new(HashMap::new()),
        })
    }

    /// 原始规则行（按配置顺序）；只读，不含节点地址与凭据。
    #[must_use]
    pub fn rule_lines(&self) -> &[String] {
        &self.rule_lines
    }

    /// 按规则顺序匹配目标，返回路由结果。
    ///
    /// # Errors
    ///
    /// 无规则命中、策略未定义或策略组为空时返回错误。
    pub fn route(&self, target: &Target) -> Result<Route> {
        for (rule, policy) in &self.rules {
            if rule.matches(target, &self.geo) {
                return self.resolve_route(policy);
            }
        }
        bail!("no rule matched target");
    }

    /// 更新某节点的健康状态（供健康检查任务调用）。
    pub fn set_health(&self, name: &str, latency: Option<u32>) {
        self.health.set(name, latency);
    }

    /// 读取某节点的健康状态。
    #[must_use]
    pub fn health_of(&self, name: &str) -> Option<u32> {
        self.health.get(name)
    }

    /// 锁定或解锁策略组的成员。
    ///
    /// `node` 为 `None` 或 `"auto"` 时恢复自动策略；手动锁定优先于
    /// `url-test` / `fallback` / `load-balance` 等任何自动选路结果。
    ///
    /// # Errors
    ///
    /// 组不存在、成员不在组内或选择表被污染时返回 [`SelectionError`]。
    pub fn set_selection(
        &self,
        group_name: &str,
        node: Option<&str>,
    ) -> Result<(), SelectionError> {
        let group = self
            .groups
            .get(group_name)
            .ok_or(SelectionError::UnknownGroup)?;
        let mut guard = self
            .selections
            .write()
            .map_err(|_| SelectionError::Poisoned)?;
        match node {
            Some(name) if name != AUTO_NODE => {
                if !group.proxies.iter().any(|member| member == name) {
                    return Err(SelectionError::UnknownNode);
                }
                guard.insert(group_name.to_owned(), name.to_owned());
            }
            _ => {
                guard.remove(group_name);
            }
        }
        Ok(())
    }

    /// 热重载后继承旧路由器的手动锁定；成员已消失的锁定自动丢弃。
    pub fn inherit_selections(&self, previous: &Self) {
        let (Ok(source), Ok(mut target)) = (previous.selections.read(), self.selections.write())
        else {
            return;
        };
        for (group_name, node) in source.iter() {
            let still_valid = self
                .groups
                .get(group_name)
                .is_some_and(|group| group.proxies.iter().any(|member| member == node));
            if still_valid {
                target.insert(group_name.clone(), node.clone());
            }
        }
    }

    /// 可观测快照：策略组选路状态 + 已登记节点健康状态。
    ///
    /// TCP 探测字段由 IPC 层合并（此处保持 `None`）；载荷不含节点地址与凭据。
    #[must_use]
    pub fn node_snapshot(&self) -> NodesSnapshot {
        let health = self.health.snapshot();
        let selections = self
            .selections
            .read()
            .map(|guard| guard.clone())
            .unwrap_or_default();

        let mut groups: Vec<NodeGroupView> = self
            .groups
            .values()
            .map(|group| {
                let (mode, selected) = match selections.get(&group.name) {
                    Some(manual) => (GroupMode::Manual, manual.clone()),
                    None => (GroupMode::Auto, self.preview_member(group).to_owned()),
                };
                NodeGroupView {
                    name: group.name.clone(),
                    kind: group.kind.clone(),
                    mode,
                    selected,
                    members: group.proxies.clone(),
                }
            })
            .collect();
        groups.sort_by(|left, right| left.name.cmp(&right.name));

        let mut nodes: Vec<NodeView> = self
            .proxies
            .values()
            .map(|proxy| NodeView {
                name: proxy.name.clone(),
                health_ms: health.get(&proxy.name).copied().flatten(),
                ..NodeView::default()
            })
            .collect();
        nodes.sort_by(|left, right| left.name.cmp(&right.name));
        NodesSnapshot { groups, nodes }
    }

    /// 已登记节点的 TCP 探测目标（名称 + `host:port`），仅供内核内部使用。
    ///
    /// 地址不会出现在任何 IPC 响应中。
    #[must_use]
    pub fn probe_targets(&self) -> Vec<(String, String)> {
        let mut targets: Vec<(String, String)> = self
            .proxies
            .values()
            .map(|proxy| {
                let host = if proxy.server.contains(':') && !proxy.server.starts_with('[') {
                    format!("[{}]", proxy.server)
                } else {
                    proxy.server.clone()
                };
                (proxy.name.clone(), format!("{host}:{}", proxy.port))
            })
            .collect();
        targets.sort_by(|left, right| left.0.cmp(&right.0));
        targets
    }

    /// 快照用的「当前/下一次」成员预览，不消耗轮询计数器。
    fn preview_member<'a>(&self, group: &'a ProxyGroup) -> &'a str {
        if group.proxies.is_empty() {
            return "";
        }
        match group.kind.as_str() {
            "load-balance" => {
                let index = self.round_robin.load(Ordering::Relaxed) % group.proxies.len();
                &group.proxies[index]
            }
            "url-test" => self.pick_fastest(group),
            "fallback" => self.pick_first_alive(group),
            _ => &group.proxies[0],
        }
    }

    /// 规则条数（含规则集展开后的条目）。
    #[must_use]
    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }

    /// 收集需要健康检查的组（`url-test` / `fallback`），成员已递归展开。
    #[must_use]
    pub fn health_specs(&self) -> Vec<HealthSpec> {
        let mut specs = Vec::new();
        for group in self.groups.values() {
            if group.kind != "url-test" && group.kind != "fallback" {
                continue;
            }
            let Some(url) = &group.url else {
                tracing::warn!(
                    group = %group.name,
                    "group has no health-check url, selection falls back to first member"
                );
                continue;
            };
            let members: Vec<Proxy> = group
                .proxies
                .iter()
                .filter_map(|name| self.resolve_depth(name, 0).ok())
                .collect();
            if members.is_empty() {
                continue;
            }
            specs.push(HealthSpec {
                group: group.name.clone(),
                kind: group.kind.clone(),
                url: url.clone(),
                interval: Duration::from_secs(group.interval.unwrap_or(300)),
                tolerance: group.tolerance.unwrap_or(0),
                members,
            });
        }
        specs
    }

    fn resolve_route(&self, name: &str) -> Result<Route> {
        if name.eq_ignore_ascii_case("DIRECT") {
            return Ok(Route::Direct);
        }
        if name.eq_ignore_ascii_case("REJECT") {
            return Ok(Route::Reject);
        }
        Ok(Route::Proxy(Box::new(self.resolve_proxy(name)?)))
    }

    fn resolve_proxy(&self, name: &str) -> Result<Proxy> {
        self.resolve_depth(name, 0)
    }

    fn resolve_depth(&self, name: &str, depth: usize) -> Result<Proxy> {
        if depth > 16 {
            bail!("proxy group nesting too deep");
        }
        if let Some(manual) = self
            .selections
            .read()
            .ok()
            .and_then(|guard| guard.get(name).cloned())
        {
            return self.resolve_depth(&manual, depth + 1);
        }
        if let Some(proxy) = self.proxies.get(name) {
            return Ok(proxy.clone());
        }
        let group = self
            .groups
            .get(name)
            .with_context(|| format!("unknown policy {name}"))?;
        if group.proxies.is_empty() {
            bail!("proxy group {name} is empty");
        }
        let selected = match group.kind.as_str() {
            "load-balance" => {
                let index = self.round_robin.fetch_add(1, Ordering::Relaxed) % group.proxies.len();
                &group.proxies[index]
            }
            "url-test" => {
                let picked = self.pick_fastest(group);
                tracing::debug!(
                    group = %group.name,
                    node = %picked,
                    latency = ?self.health_of(picked),
                    "url-test selection"
                );
                picked
            }
            "fallback" => self.pick_first_alive(group),
            _ => &group.proxies[0],
        };
        self.resolve_depth(selected, depth + 1)
    }

    /// `url-test`：延迟最低者优先；全不可用时退化为首个成员。
    fn pick_fastest<'a>(&self, group: &'a ProxyGroup) -> &'a String {
        let health = self.health.snapshot();
        let mut best: Option<(&'a String, u32)> = None;
        for name in &group.proxies {
            if let Some(Some(latency)) = health.get(name)
                && best.is_none_or(|(_, current)| *latency < current)
            {
                best = Some((name, *latency));
            }
        }
        best.map_or(&group.proxies[0], |(name, _)| name)
    }

    /// `fallback`：首个健康成员优先；全不可用时退化为首个成员。
    fn pick_first_alive<'a>(&self, group: &'a ProxyGroup) -> &'a String {
        let health = self.health.snapshot();
        for name in &group.proxies {
            if matches!(health.get(name), Some(Some(_))) {
                return name;
            }
        }
        &group.proxies[0]
    }
}

fn validate_proxy(proxy: &Proxy) -> Result<()> {
    if proxy.kind != "vless" {
        bail!("proxy {} has unsupported type {}", proxy.name, proxy.kind);
    }
    if proxy.network != "ws" {
        bail!(
            "proxy {} has unsupported network {}",
            proxy.name,
            proxy.network
        );
    }
    Ok(())
}

/// 把 `RULE-SET` 展开成具体规则；其他规则原样返回。
fn expand_rule(
    rule: &Rule,
    providers: &HashMap<String, RuleProvider>,
    line: &str,
) -> Result<Vec<Rule>> {
    let Rule::RuleSet(name) = rule else {
        return Ok(vec![rule.clone()]);
    };
    let provider = providers
        .get(name)
        .with_context(|| format!("rule {line}: rule-provider {name} not found"))?;
    let content = std::fs::read_to_string(&provider.path)
        .with_context(|| format!("read rule-set {} for {name}", provider.path))?;
    let expanded = parse_rule_set(&provider.behavior, &content)
        .with_context(|| format!("parse rule-set {name} ({})", provider.path))?;
    if expanded.is_empty() {
        bail!("rule-set {name} ({}) is empty", provider.path);
    }
    Ok(expanded)
}

fn validate_geo_rule(rule: &Rule, geo: &GeoData, line: &str) -> Result<()> {
    match rule {
        Rule::GeoIp(category) => {
            if category != "cn" {
                bail!("rule {line}: only GEOIP,CN is supported");
            }
            if geo.geoip_cn.is_none() {
                bail!("rule {line} needs --geoip data");
            }
        }
        Rule::GeoSite(category) => {
            if category != "cn" {
                bail!("rule {line}: only GEOSITE,cn is supported");
            }
            if geo.geosite_cn.is_none() {
                bail!("rule {line} needs --geosite data");
            }
        }
        _ => {}
    }
    Ok(())
}

fn is_builtin(name: &str) -> bool {
    name.eq_ignore_ascii_case("DIRECT") || name.eq_ignore_ascii_case("REJECT")
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::{
        geo::{GeoIp, GeoSite},
        profile::WsOpts,
        rules::Target,
    };

    fn proxy(name: &str) -> Proxy {
        Proxy {
            name: name.to_owned(),
            kind: "vless".to_owned(),
            server: format!("{name}.example.com"),
            port: 443,
            uuid: "11111111-1111-1111-1111-111111111111".to_owned(),
            network: "ws".to_owned(),
            tls: true,
            servername: None,
            encryption: None,
            ws_opts: Some(WsOpts {
                path: "/cdn-api".to_owned(),
                headers: HashMap::new(),
            }),
            skip_cert_verify: false,
        }
    }

    fn group(name: &str, kind: &str, members: &[&str]) -> ProxyGroup {
        ProxyGroup {
            name: name.to_owned(),
            kind: kind.to_owned(),
            proxies: members.iter().map(|m| (*m).to_owned()).collect(),
            url: None,
            interval: None,
            tolerance: None,
        }
    }

    fn router_with(rules: &[&str], groups: Vec<ProxyGroup>) -> Router {
        let profile = Profile {
            proxies: vec![proxy("a"), proxy("b")],
            proxy_groups: groups,
            rule_providers: HashMap::new(),
            rules: rules.iter().map(|line| (*line).to_owned()).collect(),
        };
        Router::from_profile(profile, Arc::new(GeoData::default())).unwrap()
    }

    fn router(rules: &[&str]) -> Router {
        router_with(
            rules,
            vec![
                group("select", "select", &["a", "b"]),
                group("lb", "load-balance", &["a", "b"]),
            ],
        )
    }

    fn proxy_name(route: Route) -> String {
        match route {
            Route::Proxy(proxy) => proxy.name,
            other => panic!("expected proxy, got {other:?}"),
        }
    }

    #[test]
    fn routes_domain_to_group_member() {
        let router = router(&[
            "DOMAIN-SUFFIX,example.com,select",
            "DOMAIN-SUFFIX,cn,DIRECT",
            "MATCH,lb",
        ]);
        let route = router
            .route(&Target::Domain("www.example.com".to_owned()))
            .unwrap();
        assert_eq!(proxy_name(route), "a");

        let route = router
            .route(&Target::Domain("www.baidu.cn".to_owned()))
            .unwrap();
        assert!(matches!(route, Route::Direct));

        let route = router
            .route(&Target::Domain("other.test".to_owned()))
            .unwrap();
        assert_eq!(proxy_name(route), "a");
    }

    #[test]
    fn load_balance_round_robins() {
        let router = router(&["MATCH,lb"]);
        let target = Target::Domain("x.test".to_owned());
        assert_eq!(proxy_name(router.route(&target).unwrap()), "a");
        assert_eq!(proxy_name(router.route(&target).unwrap()), "b");
        assert_eq!(proxy_name(router.route(&target).unwrap()), "a");
    }

    #[test]
    fn manual_selection_overrides_auto_and_restores() {
        let router = router(&["MATCH,lb"]);
        let target = Target::Domain("x.test".to_owned());

        assert_eq!(
            router.set_selection("lb", Some("missing")),
            Err(SelectionError::UnknownNode)
        );
        assert_eq!(
            router.set_selection("no-such-group", Some("a")),
            Err(SelectionError::UnknownGroup)
        );

        router.set_selection("lb", Some("b")).unwrap();
        assert_eq!(proxy_name(router.route(&target).unwrap()), "b");
        assert_eq!(proxy_name(router.route(&target).unwrap()), "b");
        let view = router
            .node_snapshot()
            .groups
            .into_iter()
            .find(|group| group.name == "lb")
            .expect("lb group");
        assert_eq!(view.mode, GroupMode::Manual);
        assert_eq!(view.selected, "b");

        // auto / None 都表示恢复自动策略。
        router.set_selection("lb", Some(AUTO_NODE)).unwrap();
        assert_eq!(proxy_name(router.route(&target).unwrap()), "a");
        router.set_selection("lb", Some("b")).unwrap();
        router.set_selection("lb", None).unwrap();
        assert_eq!(proxy_name(router.route(&target).unwrap()), "b");
    }

    #[test]
    fn selection_survives_reload_but_drops_missing_members() {
        let old = router(&["MATCH,lb"]);
        old.set_selection("lb", Some("b")).unwrap();

        let next = router(&["MATCH,lb"]);
        next.inherit_selections(&old);
        assert_eq!(
            next.node_snapshot()
                .groups
                .into_iter()
                .find(|group| group.name == "lb")
                .expect("lb group")
                .mode,
            GroupMode::Manual
        );

        // 新配置不再包含 b -> 锁定自动丢弃。
        let shrunk = router_with(&["MATCH,lb"], vec![group("lb", "load-balance", &["a"])]);
        shrunk.inherit_selections(&old);
        assert_eq!(
            shrunk
                .node_snapshot()
                .groups
                .into_iter()
                .find(|group| group.name == "lb")
                .expect("lb group")
                .mode,
            GroupMode::Auto
        );
    }

    #[test]
    fn node_snapshot_is_sorted_and_redacted() {
        let router = router(&["MATCH,lb"]);
        router.set_health("b", Some(42));
        let snapshot = router.node_snapshot();
        assert_eq!(
            snapshot
                .nodes
                .iter()
                .map(|n| n.name.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        assert_eq!(snapshot.nodes[1].health_ms, Some(42));
        assert_eq!(snapshot.nodes[1].tcp_ok, None);
        // 节点地址/凭据不得进入任何序列化载荷。
        let encoded = serde_json::to_string(&snapshot).unwrap();
        for forbidden in ["example.com", "11111111", "443", "server", "uuid"] {
            assert!(!encoded.contains(forbidden), "leaked: {forbidden}");
        }
    }

    #[test]
    fn probe_targets_are_sorted_and_well_formed() {
        let targets = router(&["MATCH,lb"]).probe_targets();
        assert_eq!(
            targets,
            vec![
                ("a".to_owned(), "a.example.com:443".to_owned()),
                ("b".to_owned(), "b.example.com:443".to_owned()),
            ]
        );
    }

    #[test]
    fn url_test_picks_lowest_latency() {
        let router = router_with(
            &["MATCH,auto"],
            vec![group("auto", "url-test", &["a", "b"])],
        );
        let target = Target::Domain("x.test".to_owned());

        // 未知健康状态 -> 退化首个成员
        assert_eq!(proxy_name(router.route(&target).unwrap()), "a");

        router.set_health("a", Some(200));
        router.set_health("b", Some(50));
        assert_eq!(proxy_name(router.route(&target).unwrap()), "b");

        // b 掉线后回落到 a
        router.set_health("b", None);
        assert_eq!(proxy_name(router.route(&target).unwrap()), "a");
    }

    #[test]
    fn fallback_picks_first_alive() {
        let router = router_with(
            &["MATCH,auto"],
            vec![group("auto", "fallback", &["a", "b"])],
        );
        let target = Target::Domain("x.test".to_owned());

        router.set_health("a", None);
        router.set_health("b", Some(120));
        assert_eq!(proxy_name(router.route(&target).unwrap()), "b");

        // 全挂 -> 退化首个成员
        router.set_health("b", None);
        assert_eq!(proxy_name(router.route(&target).unwrap()), "a");
    }

    #[test]
    fn rejects_unknown_policy() {
        let profile = Profile {
            proxies: vec![proxy("a")],
            proxy_groups: Vec::new(),
            rule_providers: HashMap::new(),
            rules: vec!["MATCH,missing".to_owned()],
        };
        assert!(Router::from_profile(profile, Arc::new(GeoData::default())).is_err());
    }

    #[test]
    fn reject_builtin_is_supported() {
        let router = router(&["DOMAIN-SUFFIX,ads.example,REJECT", "MATCH,DIRECT"]);
        let route = router
            .route(&Target::Domain("ads.example".to_owned()))
            .unwrap();
        assert!(matches!(route, Route::Reject));
        let route = router
            .route(&Target::Domain("other.test".to_owned()))
            .unwrap();
        assert!(matches!(route, Route::Direct));
    }

    #[test]
    fn geo_rules_require_data() {
        let profile = Profile {
            proxies: vec![proxy("a")],
            proxy_groups: Vec::new(),
            rule_providers: HashMap::new(),
            rules: vec!["GEOIP,CN,DIRECT".to_owned(), "MATCH,a".to_owned()],
        };
        assert!(Router::from_profile(profile, Arc::new(GeoData::default())).is_err());
    }

    #[test]
    fn geo_rules_route_with_data() {
        let profile = Profile {
            proxies: vec![proxy("a")],
            proxy_groups: Vec::new(),
            rule_providers: HashMap::new(),
            rules: vec![
                "GEOIP,CN,DIRECT".to_owned(),
                "GEOSITE,cn,DIRECT".to_owned(),
                "MATCH,a".to_owned(),
            ],
        };
        let geo = GeoData {
            geoip_cn: Some(GeoIp::parse("10.0.0.0/8\n")),
            geosite_cn: Some(GeoSite::parse("domain:example.cn\n")),
        };
        let router = Router::from_profile(profile, Arc::new(geo)).unwrap();
        assert!(matches!(
            router
                .route(&Target::Ip("10.1.2.3".parse().unwrap()))
                .unwrap(),
            Route::Direct
        ));
        assert!(matches!(
            router
                .route(&Target::Domain("www.example.cn".to_owned()))
                .unwrap(),
            Route::Direct
        ));
        assert_eq!(
            proxy_name(
                router
                    .route(&Target::Domain("www.example.com".to_owned()))
                    .unwrap()
            ),
            "a"
        );
    }

    #[test]
    fn rule_set_domain_is_expanded() {
        let path = std::env::temp_dir().join("client-core-ruleset-domain.txt");
        std::fs::write(
            &path,
            "# comment\n+.ads.example.com\n*.tracker.test\nplain.test\n",
        )
        .unwrap();

        let profile = Profile {
            proxies: vec![proxy("a")],
            proxy_groups: Vec::new(),
            rule_providers: HashMap::from([(
                "ads".to_owned(),
                RuleProvider {
                    behavior: "domain".to_owned(),
                    path: path.to_string_lossy().into_owned(),
                },
            )]),
            rules: vec!["RULE-SET,ads,REJECT".to_owned(), "MATCH,a".to_owned()],
        };
        let router = Router::from_profile(profile, Arc::new(GeoData::default())).unwrap();
        assert_eq!(router.rule_count(), 4); // 3 条展开 + MATCH

        for domain in ["ads.example.com", "x.ads.example.com", "tracker.test"] {
            assert!(matches!(
                router.route(&Target::Domain(domain.to_owned())).unwrap(),
                Route::Reject
            ));
        }
        assert_eq!(
            proxy_name(
                router
                    .route(&Target::Domain("other.test".to_owned()))
                    .unwrap()
            ),
            "a"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn rule_set_ipcidr_is_expanded() {
        let path = std::env::temp_dir().join("client-core-ruleset-ipcidr.txt");
        std::fs::write(&path, "10.0.0.0/8\n192.168.0.0/16\n").unwrap();

        let profile = Profile {
            proxies: vec![proxy("a")],
            proxy_groups: Vec::new(),
            rule_providers: HashMap::from([(
                "lan".to_owned(),
                RuleProvider {
                    behavior: "ipcidr".to_owned(),
                    path: path.to_string_lossy().into_owned(),
                },
            )]),
            rules: vec!["RULE-SET,lan,DIRECT".to_owned(), "MATCH,a".to_owned()],
        };
        let router = Router::from_profile(profile, Arc::new(GeoData::default())).unwrap();
        assert!(matches!(
            router
                .route(&Target::Ip("10.1.2.3".parse().unwrap()))
                .unwrap(),
            Route::Direct
        ));
        assert!(matches!(
            router
                .route(&Target::Ip("192.168.1.1".parse().unwrap()))
                .unwrap(),
            Route::Direct
        ));
        assert_eq!(
            proxy_name(
                router
                    .route(&Target::Ip("8.8.8.8".parse().unwrap()))
                    .unwrap()
            ),
            "a"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn rule_set_classical_is_expanded() {
        let path = std::env::temp_dir().join("client-core-ruleset-classical.txt");
        std::fs::write(
            &path,
            "DOMAIN-SUFFIX,classical.test\nIP-CIDR,172.16.0.0/12\nDOMAIN-KEYWORD,spam\n",
        )
        .unwrap();

        let profile = Profile {
            proxies: vec![proxy("a")],
            proxy_groups: Vec::new(),
            rule_providers: HashMap::from([(
                "mixed".to_owned(),
                RuleProvider {
                    behavior: "classical".to_owned(),
                    path: path.to_string_lossy().into_owned(),
                },
            )]),
            rules: vec!["RULE-SET,mixed,REJECT".to_owned(), "MATCH,a".to_owned()],
        };
        let router = Router::from_profile(profile, Arc::new(GeoData::default())).unwrap();
        assert!(matches!(
            router
                .route(&Target::Domain("x.classical.test".to_owned()))
                .unwrap(),
            Route::Reject
        ));
        assert!(matches!(
            router
                .route(&Target::Ip("172.20.0.1".parse().unwrap()))
                .unwrap(),
            Route::Reject
        ));
        assert!(matches!(
            router
                .route(&Target::Domain("spam.example".to_owned()))
                .unwrap(),
            Route::Reject
        ));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn missing_rule_set_provider_errors() {
        let profile = Profile {
            proxies: vec![proxy("a")],
            proxy_groups: Vec::new(),
            rule_providers: HashMap::new(),
            rules: vec!["RULE-SET,absent,REJECT".to_owned(), "MATCH,a".to_owned()],
        };
        assert!(Router::from_profile(profile, Arc::new(GeoData::default())).is_err());
    }
}
