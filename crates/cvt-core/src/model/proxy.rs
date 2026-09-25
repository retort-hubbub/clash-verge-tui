//! Proxy and proxy-group models.
//!
//! mihomo proxy entries are strongly typed per protocol, with well over a
//! hundred distinct keys across all supported protocols. Modelling every one
//! would be a maintenance burden and would silently drop keys on round-trip, so
//! [`Proxy`] and [`ProxyGroup`] capture the fields the application actually
//! reasons about and keep **every remaining key** in `extra`.
//!
//! The invariant that matters: `parse(render(x)) == x` for any input, including
//! protocols and options this crate has never heard of.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// A single proxy (node).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Proxy {
    /// Display name; unique within a configuration.
    pub name: String,
    /// Protocol id, e.g. `vless`, `trojan`, `hysteria2`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Remote host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    /// Remote port.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Whether the node forwards UDP.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub udp: Option<bool>,
    /// All other protocol-specific keys, preserved verbatim.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Proxy {
    /// `server:port`, or whichever half is known.
    #[must_use]
    pub fn endpoint(&self) -> String {
        match (&self.server, self.port) {
            (Some(s), Some(p)) => format!("{s}:{p}"),
            (Some(s), None) => s.clone(),
            (None, Some(p)) => format!(":{p}"),
            (None, None) => "-".to_owned(),
        }
    }

    /// `true` when the protocol speaks UDP by default and it was not disabled.
    #[must_use]
    pub fn udp_enabled(&self) -> bool {
        self.udp.unwrap_or(false)
    }
}

/// What a proxy group does when it has to choose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum GroupKind {
    /// The user picks manually.
    Select,
    /// Lowest latency above the tolerance window wins.
    UrlTest,
    /// First healthy member in declaration order.
    Fallback,
    /// Distributes by a hash or round-robin strategy.
    LoadBalance,
    /// Chains members in order.
    Relay,
    /// A kind this build does not know about.
    ///
    /// There is no `smart` variant: `clash-verge-rev` accepts one, mihomo does
    /// not, and a validator that passes a group the core will refuse to load
    /// is worse than one that names the mistake. `E-GROUP-TYPE` reports it.
    Unknown,
}

impl GroupKind {
    /// Classify a group by the `type` spelling used in configuration files.
    ///
    /// That is `select`, `url-test` and friends, as written by a user.
    #[must_use]
    pub fn from_wire(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "select" => Self::Select,
            "url-test" => Self::UrlTest,
            "fallback" => Self::Fallback,
            "load-balance" => Self::LoadBalance,
            "relay" => Self::Relay,
            _ => Self::Unknown,
        }
    }

    /// Classify a group by the **adapter type** the core reports.
    ///
    /// `GET /proxies` returns Go type names — `Selector`, `URLTest`,
    /// `LoadBalance` — not the configuration spelling, so a caller that feeds
    /// those to [`GroupKind::from_wire`] gets [`GroupKind::Unknown`] and loses
    /// the ability to tell a selectable group from a load balancer. Both
    /// spellings are therefore supported explicitly rather than guessed at.
    #[must_use]
    pub fn from_adapter(s: &str) -> Self {
        match s {
            "Selector" => Self::Select,
            "URLTest" => Self::UrlTest,
            "Fallback" => Self::Fallback,
            "LoadBalance" => Self::LoadBalance,
            "Relay" => Self::Relay,
            // Tolerate a caller that passes the configuration spelling anyway.
            other => Self::from_wire(other),
        }
    }

    /// Short human label used in the TUI.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Select => "select",
            Self::UrlTest => "url-test",
            Self::Fallback => "fallback",
            Self::LoadBalance => "load-balance",
            Self::Relay => "relay",
            Self::Unknown => "?",
        }
    }

    /// Whether the group supports latency testing its members.
    #[must_use]
    pub fn is_testable(self) -> bool {
        matches!(self, Self::UrlTest | Self::Fallback | Self::LoadBalance)
    }

    /// Whether a member can be pinned with `PUT /proxies/{name}`.
    ///
    /// The core's `SelectAble` covers exactly `Selector`, `URLTest` and
    /// `Fallback`; a `LoadBalance` group answers `400 Must be a Selector`, and
    /// the plain proxies are not groups at all.
    #[must_use]
    pub fn is_selectable(self) -> bool {
        matches!(self, Self::Select | Self::UrlTest | Self::Fallback)
    }
}

/// A proxy group.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProxyGroup {
    /// Group name; referenced by rules and by `use`/`proxies` of other groups.
    pub name: String,
    /// Group behaviour, as written in the config.
    #[serde(rename = "type")]
    pub kind: String,
    /// Explicitly listed members.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub proxies: Vec<String>,
    /// Provider names whose nodes are pulled in dynamically.
    #[serde(rename = "use", default, skip_serializing_if = "Vec::is_empty")]
    pub use_providers: Vec<String>,
    /// Health-check URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Health-check interval in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval: Option<u64>,
    /// Latency difference tolerated before switching, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tolerance: Option<u32>,
    /// Only test when the group is in use.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lazy: Option<bool>,
    /// Regex selecting provider nodes by name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    /// Expected HTTP status for the health check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_status: Option<String>,
    /// All other keys (`strategy`, `disable-udp`, `hidden`, ...).
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl ProxyGroup {
    /// Create an empty `select` group.
    #[must_use]
    pub fn new_select(name: impl Into<String>, members: Vec<String>) -> Self {
        Self {
            name: name.into(),
            kind: "select".to_owned(),
            proxies: members,
            use_providers: Vec::new(),
            url: None,
            interval: None,
            tolerance: None,
            lazy: None,
            filter: None,
            expected_status: None,
            extra: Map::new(),
        }
    }

    /// The classified behaviour of this group.
    #[must_use]
    pub fn group_kind(&self) -> GroupKind {
        GroupKind::from_wire(&self.kind)
    }

    /// `true` when membership is resolved at runtime from providers.
    #[must_use]
    pub fn is_dynamic(&self) -> bool {
        !self.use_providers.is_empty()
    }

    /// `true` when the group's member list is generated by a regex filter,
    /// in which case mihomo resolves it and the panel must not inline nodes.
    #[must_use]
    pub fn is_filtered(&self) -> bool {
        self.filter.as_ref().is_some_and(|f| !f.trim().is_empty())
    }
}

/// A named collection of nodes fetched from one URL, i.e. `proxy-providers`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProxyProvider {
    /// Provider type: `http` or `file`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Source URL for `http` providers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Source path for `file` providers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Refresh interval in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval: Option<u64>,
    /// Health-check settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health_check: Option<HealthCheck>,
    /// Extra keys (`filter`, `exclude-filter`, `exclude-type`, `override`, ...).
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Health-check block shared by rule and proxy providers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthCheck {
    /// Whether the check is enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable: Option<bool>,
    /// URL probed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Interval in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval: Option<u64>,
    /// Lazy evaluation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lazy: Option<bool>,
    /// Expected status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_status: Option<String>,
    /// Extra keys.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Aggregate view over everything selectable, used by the proxies screen.
#[derive(Debug, Clone, Default)]
pub struct ProxyInventory {
    /// Direct proxies declared in the config.
    pub proxies: Vec<Proxy>,
    /// Groups declared in the config.
    pub groups: Vec<ProxyGroup>,
    /// Rule/provider-supplied node collections.
    pub providers: BTreeMap<String, ProxyProvider>,
}

impl ProxyInventory {
    /// Total nodes, counting only declared proxies.
    #[must_use]
    pub fn proxy_count(&self) -> usize {
        self.proxies.len()
    }

    /// Look up a proxy by exact name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Proxy> {
        self.proxies.iter().find(|p| p.name == name)
    }

    /// Look up a group by exact name.
    #[must_use]
    pub fn group(&self, name: &str) -> Option<&ProxyGroup> {
        self.groups.iter().find(|g| g.name == name)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn keeps_unknown_protocol_keys() {
        let yaml = r#"
name: "JP 01"
type: vless
server: 1.2.3.4
port: 443
uuid: abc
flow: xtls-rprx-vision
reality-opts:
  public-key: KEY
  short-id: "00"
client-fingerprint: chrome
"#;
        let p: Proxy = serde_norway::from_str(yaml).unwrap();
        assert_eq!(p.kind, "vless");
        assert_eq!(p.endpoint(), "1.2.3.4:443");
        assert_eq!(p.extra.get("flow").unwrap(), "xtls-rprx-vision");
        assert!(p.extra.contains_key("reality-opts"));

        let back = serde_norway::to_string(&p).unwrap();
        assert!(back.contains("flow: xtls-rprx-vision"));
        assert!(back.contains("public-key: KEY"));
    }

    #[test]
    fn classifies_group_kinds_from_config_spelling() {
        assert_eq!(GroupKind::from_wire("url-test"), GroupKind::UrlTest);
        assert_eq!(GroupKind::from_wire("load-balance"), GroupKind::LoadBalance);
        assert_eq!(GroupKind::from_wire("nonsense"), GroupKind::Unknown);
        assert!(GroupKind::UrlTest.is_testable());
        assert!(!GroupKind::Select.is_testable());
        // Selection and testing are deliberately different sets.
        assert!(GroupKind::Select.is_selectable());
        assert!(GroupKind::UrlTest.is_selectable());
        assert!(GroupKind::Fallback.is_selectable());
        assert!(!GroupKind::LoadBalance.is_selectable());
        assert!(!GroupKind::Relay.is_selectable());
        assert!(!GroupKind::Unknown.is_selectable());
    }

    #[test]
    fn classifies_group_kinds_from_core_adapter_names() {
        // The core reports Go type names, which the configuration spelling
        // does not match.
        assert_eq!(GroupKind::from_adapter("Selector"), GroupKind::Select);
        assert_eq!(GroupKind::from_adapter("URLTest"), GroupKind::UrlTest);
        assert_eq!(GroupKind::from_adapter("Fallback"), GroupKind::Fallback);
        assert_eq!(
            GroupKind::from_adapter("LoadBalance"),
            GroupKind::LoadBalance
        );
        assert_eq!(GroupKind::from_adapter("Relay"), GroupKind::Relay);
        assert_eq!(GroupKind::from_adapter("SomethingNew"), GroupKind::Unknown);
        // Feeding adapter names to the config parser is exactly the mistake
        // this exists to prevent.
        assert_eq!(GroupKind::from_wire("Selector"), GroupKind::Unknown);
        // But a caller that passes config spelling anyway is still understood.
        assert_eq!(GroupKind::from_adapter("url-test"), GroupKind::UrlTest);
    }

    #[test]
    fn detects_dynamic_and_filtered_groups() {
        let g = ProxyGroup {
            name: "auto".into(),
            kind: "url-test".into(),
            proxies: vec![],
            use_providers: vec!["sub".into()],
            url: None,
            interval: None,
            tolerance: None,
            lazy: None,
            filter: Some("(?i)hk".into()),
            expected_status: None,
            extra: Map::new(),
        };
        assert!(g.is_dynamic());
        assert!(g.is_filtered());
    }
}
