//! One type per list.
//!
//! The core returns nested, verbose structures; a terminal column is twelve
//! characters wide. These types are the translation: they keep exactly the
//! fields a row displays or an action needs, and drop the rest.
//!
//! Keeping them separate from the core's types is what allows the interface to
//! show something sensible while a value is missing, and it is why the
//! rendering code never has to reach into a nested `serde_json::Value`.

use cvt_core::mihomo::types::{Connection, LogEvent, ProxyView, RuleInfo, Traffic};
use cvt_core::model::config::Config;
use cvt_core::model::proxy::GroupKind;
use cvt_core::profile::item::{PrfItem, ProfileType};
use cvt_core::profile::store::ProfileStore;

use crate::state::{Filterable, contains_ignore_case, human_bytes, human_delay};

/// A log line as displayed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRow {
    /// Local time the line was received, `HH:MM:SS`.
    pub at: String,
    /// Severity as the core spells it.
    pub level: String,
    /// The message text.
    pub message: String,
}

impl LogRow {
    /// Build a row, stamping it with the current local time.
    #[must_use]
    pub fn new(level: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            at: chrono::Local::now().format("%H:%M:%S").to_string(),
            level: level.into(),
            message: message.into(),
        }
    }

    /// Build a row from a core log frame.
    #[must_use]
    pub fn from_event(event: &LogEvent) -> Self {
        Self::new(event.level.clone(), event.payload.clone())
    }

    /// Width in columns of the rendered line.
    #[must_use]
    pub fn display_width(&self) -> usize {
        self.at.chars().count() + self.level.chars().count() + self.message.chars().count() + 2
    }
}

impl Filterable for LogRow {
    fn matches_filter(&self, needle: &str) -> bool {
        contains_ignore_case(&self.message, needle) || contains_ignore_case(&self.level, needle)
    }
}

/// The live readings shown on the dashboard.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Live {
    /// Bytes per second downloaded.
    pub down_rate: u64,
    /// Bytes per second uploaded.
    pub up_rate: u64,
    /// Cumulative bytes downloaded.
    pub down_total: u64,
    /// Cumulative bytes uploaded.
    pub up_total: u64,
    /// Resident memory in bytes.
    pub memory: u64,
    /// Connections currently open.
    pub connections: usize,
}

impl Live {
    /// Apply a traffic sample.
    pub fn absorb_traffic(&mut self, traffic: Traffic) {
        self.down_rate = traffic.down;
        self.up_rate = traffic.up;
        self.down_total = traffic.down_total;
        self.up_total = traffic.up_total;
    }

    /// Total bytes transferred in both directions.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.up_total.saturating_add(self.down_total)
    }

    /// A one-line summary for a status bar.
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "↓ {}/s  ↑ {}/s   total {}",
            human_bytes(self.down_rate),
            human_bytes(self.up_rate),
            human_bytes(self.total())
        )
    }
}

/// One row of the profiles list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileRow {
    /// Profile uid.
    pub uid: String,
    /// Display name.
    pub name: String,
    /// What the profile contributes.
    pub kind: ProfileType,
    /// Whether it supplies the base document.
    pub current: bool,
    /// Whether it is in the explicit chain.
    pub in_chain: bool,
    /// Unix time of the last successful update.
    pub updated: Option<i64>,
    /// Subscription URL, for remote profiles.
    pub url: Option<String>,
    /// Subscription quota, when the provider reported it.
    pub quota: cvt_core::profile::item::UserInfo,
    /// Why the profile cannot run, if it cannot.
    pub unsupported: Option<String>,
    /// Number of edits, for patch profiles; `None` when not applicable.
    pub edits: Option<usize>,
}

impl ProfileRow {
    /// Build a row from an index entry.
    #[must_use]
    pub fn from_item(item: &PrfItem, current: bool, in_chain: bool) -> Self {
        Self {
            uid: item.uid.clone(),
            name: item.label().to_owned(),
            kind: item.kind,
            current,
            in_chain,
            updated: item.updated,
            url: item.url.clone(),
            quota: item.extra,
            unsupported: item.unsupported_reason(),
            edits: None,
        }
    }

    /// Build every row for a store, marking the current and chained profiles.
    #[must_use]
    pub fn all(store: &ProfileStore) -> Vec<Self> {
        let current = store.current_uid();
        let chain = &store.index().chain;
        store
            .items()
            .iter()
            .map(|item| {
                Self::from_item(
                    item,
                    current == Some(item.uid.as_str()),
                    chain.contains(&item.uid),
                )
            })
            .collect()
    }

    /// How the age of the last update reads.
    #[must_use]
    pub fn updated_label(&self, now: i64) -> String {
        match self.updated {
            None if self.kind == ProfileType::Remote => "never".to_owned(),
            None => "-".to_owned(),
            Some(t) => crate::state::human_age(now.saturating_sub(t)),
        }
    }

    /// Quota usage as a percentage, when the provider reported a total.
    #[must_use]
    pub fn quota_label(&self) -> Option<String> {
        let fraction = self.quota.used_fraction()?;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        Some(format!("{:.0}%", fraction * 100.0))
    }

    /// Whether this profile is usable in the chain.
    #[must_use]
    pub fn is_usable(&self) -> bool {
        self.unsupported.is_none()
    }

    /// The role shown in the list, e.g. `remote (current)`.
    #[must_use]
    pub fn role_label(&self) -> String {
        let base = self.kind.as_str();
        match (self.current, self.in_chain) {
            (true, _) => format!("{base} · current"),
            (false, true) => format!("{base} · in chain"),
            (false, false) if self.kind.is_patch() => format!("{base} · excluded"),
            _ => base.to_owned(),
        }
    }
}

impl Filterable for ProfileRow {
    fn matches_filter(&self, needle: &str) -> bool {
        contains_ignore_case(&self.name, needle)
            || contains_ignore_case(&self.uid, needle)
            || contains_ignore_case(self.kind.as_str(), needle)
            || self
                .url
                .as_deref()
                .is_some_and(|u| contains_ignore_case(u, needle))
    }
}

/// One row of the proxies list: either a group or a node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeRow {
    /// Node or group name.
    pub name: String,
    /// Adapter type, e.g. `Selector` or `Vless`.
    pub kind: String,
    /// Owning group, for a member row.
    pub group: Option<String>,
    /// Most recent latency reading, `Some(0)` when never measured.
    pub delay: Option<u16>,
    /// Last health-check result.
    pub alive: bool,
    /// Whether this is the group's current choice.
    pub active: bool,
    /// `true` for group rows.
    pub is_group: bool,
    /// Member count, for groups.
    pub members: usize,
    /// Whether a selection can be pinned here.
    pub selectable: bool,
}

impl NodeRow {
    /// Build a group row.
    #[must_use]
    pub fn from_group(view: &ProxyView) -> Self {
        Self {
            name: view.name.clone(),
            kind: view.kind.clone(),
            group: None,
            delay: view.latest_delay(),
            alive: view.alive,
            active: false,
            is_group: true,
            members: view.members().len(),
            selectable: view.is_selectable(),
        }
    }

    /// Build a member row.
    #[must_use]
    pub fn from_member(view: &ProxyView, group: &str, active: bool) -> Self {
        Self {
            name: view.name.clone(),
            kind: view.kind.clone(),
            group: Some(group.to_owned()),
            delay: view.latest_delay(),
            alive: view.alive,
            active,
            is_group: false,
            members: 0,
            selectable: true,
        }
    }

    /// Build a row for a proxy declared in the configuration, before the core
    /// has reported anything. Used when the core is not running, so that the
    /// user still sees what they configured.
    #[must_use]
    pub fn from_config_proxy(proxy: &cvt_core::model::proxy::Proxy) -> Self {
        Self {
            name: proxy.name.clone(),
            kind: proxy.kind.clone(),
            group: None,
            delay: None,
            alive: false,
            active: false,
            is_group: false,
            members: 0,
            selectable: false,
        }
    }

    /// Latency formatted for a column.
    #[must_use]
    pub fn delay_label(&self) -> String {
        human_delay(self.delay)
    }

    /// A short description of the group behaviour, for a group row.
    ///
    /// `kind` holds the core's adapter name (`Selector`), so the adapter
    /// classifier is the correct one here — the configuration-spelling parser
    /// would report every group as unknown.
    #[must_use]
    pub fn group_kind_label(&self) -> &'static str {
        if self.is_group {
            GroupKind::from_adapter(&self.kind).label()
        } else {
            "-"
        }
    }
}

impl Filterable for NodeRow {
    fn matches_filter(&self, needle: &str) -> bool {
        contains_ignore_case(&self.name, needle)
            || contains_ignore_case(&self.kind, needle)
            || self
                .group
                .as_deref()
                .is_some_and(|g| contains_ignore_case(g, needle))
    }
}

/// One row of the connections list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionRow {
    /// Connection id, needed to close it.
    pub id: String,
    /// Destination, preferring the sniffed host.
    pub destination: String,
    /// `tcp` or `udp`.
    pub network: String,
    /// Owning process, empty when process matching is off.
    pub process: String,
    /// Matched rule, rendered in configuration spelling.
    pub rule: String,
    /// Proxy chain, outermost group first for display.
    pub chain: String,
    /// Bytes sent by the client.
    pub upload: u64,
    /// Bytes received from the remote.
    pub download: u64,
    /// Start time, `HH:MM:SS`.
    pub started: String,
}

impl ConnectionRow {
    /// Build a row from a core connection.
    #[must_use]
    pub fn from_connection(connection: &Connection) -> Self {
        let meta = connection.meta();
        // Chains arrive outbound-first; the user thinks in terms of the group
        // the rule selected, so display them the other way round.
        let chain = connection
            .chains
            .iter()
            .rev()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join(" → ");
        Self {
            id: connection.id.clone(),
            destination: meta.destination_endpoint(),
            network: meta.network.clone(),
            process: if meta.process.is_empty() {
                meta.process_path
            } else {
                meta.process
            },
            rule: connection.rule_label(),
            chain,
            upload: connection.upload,
            download: connection.download,
            started: connection
                .start
                .split('T')
                .nth(1)
                .map_or_else(String::new, |t| t.chars().take(8).collect()),
        }
    }

    /// Total bytes, both directions.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.upload.saturating_add(self.download)
    }

    /// Traffic formatted for a column.
    #[must_use]
    pub fn traffic_label(&self) -> String {
        format!(
            "↑{} ↓{}",
            human_bytes(self.upload),
            human_bytes(self.download)
        )
    }
}

impl Filterable for ConnectionRow {
    fn matches_filter(&self, needle: &str) -> bool {
        contains_ignore_case(&self.destination, needle)
            || contains_ignore_case(&self.process, needle)
            || contains_ignore_case(&self.rule, needle)
            || contains_ignore_case(&self.chain, needle)
            || contains_ignore_case(&self.network, needle)
    }
}

/// One row of the rules list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleRow {
    /// Position in the running configuration, used to toggle it.
    pub index: u32,
    /// Rule type in configuration spelling.
    pub kind: String,
    /// The value the rule matches.
    pub payload: String,
    /// Target policy.
    pub policy: String,
    /// Whether it is currently disabled.
    pub disabled: bool,
    /// Times it matched.
    pub hits: u64,
    /// Times it was evaluated without matching.
    pub misses: u64,
    /// The rule as it would be written in a config file.
    pub raw: String,
}

impl RuleRow {
    /// Build a row from a core rule.
    #[must_use]
    pub fn from_rule(rule: &RuleInfo) -> Self {
        let stats = rule.stats();
        Self {
            index: rule.index,
            kind: cvt_core::mihomo::types::rule_kind_to_config(&rule.kind),
            payload: rule.payload.clone(),
            policy: rule.proxy.clone(),
            disabled: stats.disabled,
            hits: stats.hit_count,
            misses: stats.miss_count,
            raw: rule.as_config_rule(),
        }
    }

    /// Build a row from a rule read out of the configuration file, where no
    /// counters exist because the core is not running.
    #[must_use]
    pub fn from_config_rule(rule: &cvt_core::model::rule::Rule, index: u32) -> Self {
        Self {
            index,
            kind: rule.kind.clone(),
            payload: rule.payload.clone().unwrap_or_default(),
            policy: rule.policy.clone(),
            disabled: false,
            hits: 0,
            misses: 0,
            raw: rule.to_string(),
        }
    }

    /// Whether the core reports counters for this rule.
    #[must_use]
    pub fn has_stats(&self) -> bool {
        self.hits > 0 || self.misses > 0
    }

    /// Whether the rule has been evaluated plenty of times and never fired,
    /// which usually means it is shadowed by an earlier rule or simply unused.
    #[must_use]
    pub fn looks_dead(&self, minimum_evaluations: u64) -> bool {
        !self.disabled && self.hits == 0 && self.misses >= minimum_evaluations
    }

    /// Hit count formatted for a column.
    #[must_use]
    pub fn hits_label(&self) -> String {
        if !self.has_stats() {
            "-".to_owned()
        } else if self.hits == 0 {
            format!("0/{}", self.misses)
        } else {
            self.hits.to_string()
        }
    }
}

impl Filterable for RuleRow {
    fn matches_filter(&self, needle: &str) -> bool {
        contains_ignore_case(&self.raw, needle)
            || contains_ignore_case(&self.kind, needle)
            || contains_ignore_case(&self.payload, needle)
            || contains_ignore_case(&self.policy, needle)
    }
}

/// One row of the tests list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestRow {
    /// Which test this is.
    pub kind: TestKind,
    /// What is being tested: a group, a node, or the whole configuration.
    pub target: String,
    /// The most recent result.
    pub result: TestResult,
}

/// The kinds of check the tests screen offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestKind {
    /// Measure every member of a group.
    GroupLatency,
    /// Measure one node.
    NodeLatency,
    /// Re-read the core's version and capabilities.
    CoreHealth,
    /// Resolve a name through the core's own resolver.
    DnsLookup,
}

impl TestKind {
    /// Every kind, in the order the screen lists them.
    #[must_use]
    pub fn all() -> [Self; 4] {
        [
            Self::GroupLatency,
            Self::NodeLatency,
            Self::CoreHealth,
            Self::DnsLookup,
        ]
    }

    /// The label shown in the list.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::GroupLatency => "group latency",
            Self::NodeLatency => "node latency",
            Self::CoreHealth => "core health",
            Self::DnsLookup => "dns lookup",
        }
    }

    /// What the test does, for the detail pane.
    #[must_use]
    pub fn description(self) -> &'static str {
        match self {
            Self::GroupLatency => {
                "asks the core to test every member of a group through its own health-check URL"
            }
            Self::NodeLatency => "opens a connection to the node and measures how long it takes",
            Self::CoreHealth => "reads the core's version and reports which optional routes exist",
            Self::DnsLookup => {
                "resolves a name through the core's resolver, so fake-IP mode shows the synthetic address"
            }
        }
    }
}

/// How a test ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestResult {
    /// Not run yet.
    Pending,
    /// Running now.
    Running,
    /// Finished successfully.
    Passed(String),
    /// Finished unsuccessfully.
    Failed(String),
}

impl TestResult {
    /// A short label for the status column.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Pending => "pending".to_owned(),
            Self::Running => "running".to_owned(),
            Self::Passed(s) => s.clone(),
            Self::Failed(e) => format!("failed: {e}"),
        }
    }

    /// Whether the test finished, either way.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        matches!(self, Self::Passed(_) | Self::Failed(_))
    }
}

impl Filterable for TestRow {
    fn matches_filter(&self, needle: &str) -> bool {
        contains_ignore_case(self.kind.label(), needle)
            || contains_ignore_case(&self.target, needle)
    }
}

/// Build the proxies list from a configuration, for when no core is running.
///
/// Groups come first, each followed by its declared members, so the shape of
/// the configuration is visible before anything is started.
#[must_use]
pub fn nodes_from_config(config: &Config) -> Vec<NodeRow> {
    let proxies = config.proxies();
    let mut out = Vec::new();
    for group in config.proxy_groups() {
        out.push(NodeRow {
            name: group.name.clone(),
            kind: group.kind.clone(),
            group: None,
            delay: None,
            alive: false,
            active: false,
            is_group: true,
            members: group.proxies.len(),
            selectable: group.group_kind().is_selectable(),
        });
        for member in &group.proxies {
            let kind = proxies
                .iter()
                .find(|p| p.name == *member)
                .map_or_else(|| "-".to_owned(), |p| p.kind.clone());
            out.push(NodeRow {
                name: member.clone(),
                kind,
                group: Some(group.name.clone()),
                delay: None,
                alive: false,
                active: false,
                is_group: false,
                members: 0,
                selectable: false,
            });
        }
    }
    // Nodes declared but not referenced by any group are still worth showing:
    // an unreferenced proxy is usually a configuration mistake, and hiding it
    // would hide the mistake too.
    let grouped: Vec<String> = out
        .iter()
        .filter(|r| r.group.is_some())
        .map(|r| r.name.clone())
        .collect();
    for proxy in &proxies {
        if !grouped.iter().any(|n| n == &proxy.name) {
            out.push(NodeRow::from_config_proxy(proxy));
        }
    }
    out
}

/// A group and its members, as the core reports them.
#[must_use]
pub fn nodes_from_core(groups: &[ProxyView], all: &[ProxyView]) -> Vec<NodeRow> {
    let mut out: Vec<NodeRow> = Vec::new();
    for group in groups {
        out.push(NodeRow::from_group(group));
        let current = group.now.clone().unwrap_or_default();
        for member in group.members() {
            let view = all.iter().find(|p| p.name == *member);
            let row = match view {
                Some(v) => NodeRow::from_member(v, &group.name, *member == current),
                None => NodeRow {
                    name: member.clone(),
                    kind: "-".to_owned(),
                    group: Some(group.name.clone()),
                    delay: None,
                    alive: false,
                    active: false,
                    is_group: false,
                    members: 0,
                    selectable: false,
                },
            };
            out.push(row);
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    fn view(value: serde_json::Value) -> ProxyView {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn a_direct_proxy_row_has_no_group_and_is_not_selectable() {
        let row = NodeRow::from_group(&view(json!({
            "name": "DIRECT", "type": "Direct", "alive": true, "history": []
        })));
        // `from_group` is used for anything from `/proxies`; a plain proxy has
        // no `all`, so it reports zero members.
        assert_eq!(row.members, 0);
        assert!(!row.selectable);
    }

    #[test]
    fn a_group_row_reports_its_member_count_and_selectability() {
        let row = NodeRow::from_group(&view(json!({
            "name": "PROXY", "type": "Selector", "all": ["a", "b", "c"], "now": "a"
        })));
        assert!(row.is_group);
        assert_eq!(row.members, 3);
        assert!(row.selectable);
        assert_eq!(row.group_kind_label(), "select");
    }

    #[test]
    fn a_load_balance_group_is_not_selectable() {
        // The core answers `400 Must be a Selector` for these.
        let row = NodeRow::from_group(&view(json!({
            "name": "balance", "type": "LoadBalance", "all": ["a", "b"]
        })));
        assert!(!row.selectable);
        assert_eq!(row.group_kind_label(), "load-balance");
    }

    #[test]
    fn latency_labels_distinguish_the_three_states() {
        let mut row = NodeRow::from_member(
            &view(json!({"name": "a", "type": "Vless", "history": [{"time": "t", "delay": 42}]})),
            "PROXY",
            false,
        );
        assert_eq!(row.delay_label(), "42 ms");
        row.delay = Some(0);
        assert_eq!(row.delay_label(), "?");
        row.delay = None;
        assert_eq!(row.delay_label(), "-");
    }

    #[test]
    fn a_connection_row_reverses_the_chain_for_display() {
        let connection: Connection = serde_json::from_value(json!({
            "id": "x",
            "metadata": {"network": "tcp", "host": "example.com", "destinationPort": "443",
                         "process": "curl"},
            "upload": 100, "download": 2048,
            "start": "2026-09-25T17:40:55.786450952+08:00",
            "chains": ["JP 01", "PROXY", "GLOBAL"],
            "rule": "DomainSuffix", "rulePayload": "example.com"
        }))
        .unwrap();
        let row = ConnectionRow::from_connection(&connection);
        assert_eq!(row.destination, "example.com:443");
        assert_eq!(row.process, "curl");
        assert_eq!(row.rule, "DOMAIN-SUFFIX,example.com");
        assert_eq!(
            row.chain, "GLOBAL → PROXY → JP 01",
            "the core reports outbound-first; the user reads the other way round"
        );
        assert_eq!(row.started, "17:40:55");
        assert_eq!(row.traffic_label(), "↑100 B ↓2.0 KiB");
        assert_eq!(row.total(), 2148);
    }

    #[test]
    fn a_connection_row_copes_with_missing_metadata() {
        let connection: Connection =
            serde_json::from_value(json!({"id": "y", "metadata": null, "chains": []})).unwrap();
        let row = ConnectionRow::from_connection(&connection);
        assert_eq!(row.destination, "-");
        assert!(row.chain.is_empty());
        assert_eq!(row.rule, "-");
    }

    #[test]
    fn a_rule_row_translates_pascal_case_and_reports_counters() {
        let info: RuleInfo = serde_json::from_value(json!({
            "index": 3, "type": "DomainSuffix", "payload": "google.com", "proxy": "PROXY",
            "size": -1,
            "extra": {"disabled": true, "hitCount": 5, "missCount": 9, "hitAt": "", "missAt": ""}
        }))
        .unwrap();
        let row = RuleRow::from_rule(&info);
        assert_eq!(row.kind, "DOMAIN-SUFFIX");
        assert_eq!(row.raw, "DOMAIN-SUFFIX,google.com,PROXY");
        assert!(row.disabled);
        assert_eq!(row.hits_label(), "5");
        assert!(row.has_stats());
        assert!(!row.looks_dead(1), "a disabled rule is not dead, it is off");
    }

    #[test]
    fn a_never_firing_rule_is_flagged_once_it_has_been_evaluated_enough() {
        let info: RuleInfo = serde_json::from_value(json!({
            "index": 1, "type": "Domain", "payload": "a.test", "proxy": "DIRECT", "size": -1,
            "extra": {"disabled": false, "hitCount": 0, "missCount": 5000, "hitAt": "", "missAt": ""}
        }))
        .unwrap();
        let row = RuleRow::from_rule(&info);
        assert_eq!(row.hits_label(), "0/5000");
        assert!(row.looks_dead(1000));
        assert!(!row.looks_dead(10_000));
    }

    #[test]
    fn a_rule_without_counters_says_so_rather_than_showing_zero() {
        let info: RuleInfo = serde_json::from_value(json!({
            "index": 0, "type": "Match", "payload": "", "proxy": "DIRECT", "size": -1
        }))
        .unwrap();
        let row = RuleRow::from_rule(&info);
        assert_eq!(
            row.hits_label(),
            "-",
            "a core without statistics is not a zero hit count"
        );
        assert!(!row.has_stats());
        assert_eq!(row.raw, "MATCH,DIRECT");
    }

    #[test]
    fn config_nodes_show_groups_then_their_members_then_orphans() {
        let config = Config::from_yaml(
            r#"
proxies:
  - { name: "JP 01", type: vless, server: 1.2.3.4, port: 443, uuid: u }
  - { name: "Orphan", type: trojan, server: 5.6.7.8, port: 443, password: p }
proxy-groups:
  - { name: PROXY, type: select, proxies: ["JP 01", DIRECT] }
rules: [MATCH,PROXY]
"#,
        )
        .unwrap();
        let rows = nodes_from_config(&config);
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["PROXY", "JP 01", "DIRECT", "Orphan"]);
        assert_eq!(rows[0].members, 2);
        assert_eq!(rows[1].group.as_deref(), Some("PROXY"));
        assert_eq!(rows[1].kind, "vless", "a member inherits the real protocol");
        assert!(
            rows[3].group.is_none(),
            "an unreferenced node is still listed"
        );
    }

    #[test]
    fn core_nodes_mark_the_active_member() {
        let groups = vec![view(json!({
            "name": "PROXY", "type": "Selector", "all": ["a", "b"], "now": "b"
        }))];
        let all = vec![
            view(json!({"name": "a", "type": "Vless", "history": [{"time": "t", "delay": 10}]})),
            view(json!({"name": "b", "type": "Vless", "history": [{"time": "t", "delay": 20}]})),
        ];
        let rows = nodes_from_core(&groups, &all);
        assert_eq!(rows.len(), 3);
        assert!(!rows[1].active);
        assert!(rows[2].active, "`now` marks the selected member");
        assert_eq!(rows[2].delay_label(), "20 ms");
    }

    #[test]
    fn a_member_the_core_did_not_describe_is_still_listed() {
        let groups = vec![view(
            json!({"name": "G", "type": "Selector", "all": ["ghost"]}),
        )];
        let rows = nodes_from_core(&groups, &[]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].name, "ghost");
        assert_eq!(rows[1].kind, "-");
        assert_eq!(rows[1].delay_label(), "-");
    }

    #[test]
    fn profile_rows_mark_the_current_and_chained_entries() {
        let dir = tempfile::TempDir::new().unwrap();
        let paths = cvt_core::AppPaths::new(dir.path());
        paths.ensure_dirs().unwrap();
        let mut store = ProfileStore::load(&paths).unwrap();
        store.add(PrfItem::local("L1", "base"));
        store.add(PrfItem::patch("m1", "merge", ProfileType::Merge));
        store.set_current("L1").unwrap();
        store.set_chain(&["m1".into()]).unwrap();

        let rows = ProfileRow::all(&store);
        assert_eq!(rows.len(), 2);
        assert!(rows[0].current);
        assert_eq!(rows[0].role_label(), "local · current");
        assert!(rows[1].in_chain);
        assert_eq!(rows[1].role_label(), "merge · in chain");
        assert!(rows.iter().all(ProfileRow::is_usable));
    }

    #[test]
    fn an_excluded_patch_says_so_and_a_script_says_it_is_unsupported() {
        let dir = tempfile::TempDir::new().unwrap();
        let paths = cvt_core::AppPaths::new(dir.path());
        paths.ensure_dirs().unwrap();
        let mut store = ProfileStore::load(&paths).unwrap();
        store.add(PrfItem::local("L1", "base"));
        store.add(PrfItem::patch("o1", "office", ProfileType::Override));
        store.add(PrfItem::patch("s1", "tweaks", ProfileType::Script));
        store.set_current("L1").unwrap();

        let rows = ProfileRow::all(&store);
        assert_eq!(rows[1].role_label(), "override · excluded");
        assert!(rows[1].is_usable());
        assert!(!rows[2].is_usable());
        assert!(
            rows[2]
                .unsupported
                .as_deref()
                .unwrap()
                .contains("JavaScript")
        );
    }

    #[test]
    fn an_update_age_is_rendered_against_the_current_time() {
        let mut row = ProfileRow::from_item(&PrfItem::remote("R1", "a", "https://x"), false, false);
        assert_eq!(row.updated_label(1000), "never");
        row.updated = Some(1000);
        assert_eq!(row.updated_label(1000), "just now");
        assert_eq!(row.updated_label(1000 + 7200), "2h ago");
        let patch =
            ProfileRow::from_item(&PrfItem::patch("m1", "m", ProfileType::Merge), false, false);
        assert_eq!(patch.updated_label(0), "-", "a patch has no update age");
    }

    #[test]
    fn quota_is_only_reported_when_the_provider_gave_a_total() {
        let mut item = PrfItem::remote("R1", "a", "https://x");
        item.extra = cvt_core::profile::item::UserInfo {
            upload: 1,
            download: 1,
            total: 4,
            expire: 0,
        };
        let row = ProfileRow::from_item(&item, false, false);
        assert_eq!(row.quota_label().as_deref(), Some("50%"));

        let unlimited =
            ProfileRow::from_item(&PrfItem::remote("R2", "b", "https://y"), false, false);
        assert_eq!(unlimited.quota_label(), None);
    }

    #[test]
    fn rows_filter_on_the_fields_a_user_would_search_for() {
        let row = ProfileRow::from_item(
            &PrfItem::remote("Rabc", "Tokyo", "https://sub.example/x"),
            false,
            false,
        );
        assert!(row.matches_filter("tokyo"));
        assert!(row.matches_filter("RABC"));
        assert!(row.matches_filter("remote"));
        assert!(row.matches_filter("sub.example"));
        assert!(!row.matches_filter("berlin"));
        assert!(row.matches_filter(""), "an empty needle matches everything");

        let node =
            NodeRow::from_group(&view(json!({"name": "auto", "type": "URLTest", "all": []})));
        assert!(node.matches_filter("url"));
        assert!(node.matches_filter("AUTO"));

        let conn = ConnectionRow {
            id: "x".into(),
            destination: "example.com:443".into(),
            network: "tcp".into(),
            process: "curl".into(),
            rule: "MATCH".into(),
            chain: "PROXY".into(),
            upload: 0,
            download: 0,
            started: String::new(),
        };
        assert!(conn.matches_filter("curl"));
        assert!(conn.matches_filter("443"));
        assert!(!conn.matches_filter("wget"));
    }

    #[test]
    fn test_kinds_document_themselves() {
        for kind in TestKind::all() {
            assert!(!kind.label().is_empty());
            assert!(!kind.description().is_empty());
        }
        assert_eq!(TestKind::all().len(), 4);
    }

    #[test]
    fn test_results_render_every_state() {
        assert_eq!(TestResult::Pending.label(), "pending");
        assert!(!TestResult::Pending.is_finished());
        assert_eq!(TestResult::Running.label(), "running");
        assert!(!TestResult::Running.is_finished());
        assert_eq!(TestResult::Passed("42 ms".into()).label(), "42 ms");
        assert!(TestResult::Passed(String::new()).is_finished());
        assert!(
            TestResult::Failed("timeout".into())
                .label()
                .contains("timeout")
        );
    }

    #[test]
    fn live_readings_track_traffic_without_losing_the_other_gauges() {
        let mut live = Live {
            memory: 4096,
            connections: 3,
            ..Live::default()
        };
        live.absorb_traffic(Traffic {
            up: 10,
            down: 20,
            up_total: 100,
            down_total: 200,
        });
        assert_eq!(live.up_rate, 10);
        assert_eq!(live.down_rate, 20);
        assert_eq!(live.total(), 300);
        assert_eq!(live.memory, 4096, "a traffic sample must not clear memory");
        assert_eq!(live.connections, 3);
        assert!(live.summary().contains("total 300 B"));
    }

    #[test]
    fn a_log_row_carries_a_timestamp_and_filters_on_level() {
        let row = LogRow::new("error", "dial failed");
        assert_eq!(row.at.len(), 8, "HH:MM:SS");
        assert!(row.matches_filter("dial"));
        assert!(row.matches_filter("ERROR"));
        assert!(!row.matches_filter("success"));
        assert!(row.display_width() > row.message.len());

        let from_core = LogRow::from_event(&LogEvent {
            level: "warning".into(),
            payload: "slow".into(),
        });
        assert_eq!(from_core.level, "warning");
        assert!(from_core.matches_filter("SLOW"));
    }
}
