//! Wire types for the mihomo controller API.
//!
//! Every field name here was taken from the core's own marshalling code and
//! confirmed against a live `mihomo v1.19.31`, because the published docs are
//! wrong in several places that matter:
//!
//! * `/version` has **no** `premium` key (that is the old Dreamacro/Clash field),
//! * `/logs` frames are **flat** — `{"type":"info","payload":"text"}` — with no
//!   `{"type":"log", "payload":{…}}` wrapper, and `payload` is a string,
//! * rule `type` and proxy `type` are Go **PascalCase** identifiers
//!   (`DomainSuffix`, `Vless`), not the `DOMAIN-SUFFIX`/`vless` spelling used in
//!   configuration files,
//! * connection metadata ports are JSON **strings**,
//! * `connections` is `null`, not `[]`, when nothing is connected, and
//! * a delayed group may legitimately report `0` ms while the single-proxy
//!   endpoint treats `0` as a failure.
//!
//! So the types below are defensive: `type` fields are kept as strings rather
//! than enums, every list that could be `null` is an `Option`, and anything
//! unrecognised is preserved in an `extra` map instead of being dropped.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Deserialize a `null` as the type's default.
///
/// The core sends `null` for a list it has nothing to put in it. `connections`
/// is the one the spec records, but the shape is a property of how the
/// response is built rather than of that endpoint, and a client that fails to
/// parse an idle response fails exactly when nobody is looking.
fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// Deserialize a port the core may send as a string or as a number.
fn string_or_number<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    /// The two spellings, in the order serde tries them.
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Port {
        Text(String),
        Number(serde_json::Number),
    }

    Ok(match Option::<Port>::deserialize(deserializer)? {
        Some(Port::Text(text)) => text,
        Some(Port::Number(number)) => number.to_string(),
        None => String::new(),
    })
}
use serde_json::{Map, Value};

/// `GET /`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    /// Always `mihomo` for a mihomo core.
    pub hello: String,
}

/// `GET /version`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Version {
    /// `true` on every mihomo build; the field exists for compatibility with
    /// the dashboards that check it.
    #[serde(default)]
    pub meta: bool,
    /// Version string, **including** a leading `v` in official releases.
    pub version: String,
}

impl Version {
    /// The version without a leading `v`, for comparisons and display.
    #[must_use]
    pub fn trimmed(&self) -> &str {
        self.version.strip_prefix('v').unwrap_or(&self.version)
    }

    /// Just the numeric parts, e.g. `1.19.31`. Returns `None` for a version
    /// string a custom build invented.
    #[must_use]
    pub fn semver(&self) -> Option<(u32, u32, u32)> {
        let mut it = self.trimmed().split(['.', '-', '+']);
        let major = it.next()?.parse().ok()?;
        let minor = it.next()?.parse().ok()?;
        let patch = it.next().and_then(|p| p.parse().ok()).unwrap_or(0);
        Some((major, minor, patch))
    }
}

/// One entry of a proxy's latency history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelaySample {
    /// RFC3339 timestamp of the measurement.
    #[serde(default)]
    pub time: String,
    /// Measured latency in milliseconds.
    #[serde(default)]
    pub delay: u16,
}

/// A proxy or proxy group as returned by `GET /proxies`.
///
/// Groups and plain proxies share one shape; [`Self::is_group`] distinguishes
/// them by the presence of `all`, and groups never carry an `id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProxyView {
    /// Name, unique within the running configuration.
    pub name: String,
    /// Adapter type in PascalCase, e.g. `Selector`, `Vless`, `Direct`.
    #[serde(rename = "type", default)]
    pub kind: String,
    /// Last health-check result.
    #[serde(default)]
    pub alive: bool,
    /// Recent latency measurements, oldest first.
    #[serde(default, deserialize_with = "null_as_default")]
    pub history: Vec<DelaySample>,
    /// Identifier; present on plain proxies, absent on groups.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Members, on groups only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub all: Option<Vec<String>>,
    /// Currently selected member. Absent on `LoadBalance`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub now: Option<String>,
    /// Health-check URL configured for the group.
    #[serde(rename = "testUrl", default, skip_serializing_if = "Option::is_none")]
    pub test_url: Option<String>,
    /// Accepted status expression.
    #[serde(
        rename = "expectedStatus",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub expected_status: Option<String>,
    /// Pinned member on a `URLTest`/`Fallback` group; empty means automatic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fixed: Option<String>,
    /// Whether the group is hidden from dashboards.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden: Option<bool>,
    /// Dashboard icon URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Name of the proxy used when a group would otherwise be empty.
    #[serde(
        rename = "emptyFallback",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub empty_fallback: Option<String>,
    /// Provider this proxy came from, empty when defined inline.
    #[serde(
        rename = "provider-name",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub provider_name: Option<String>,
    /// Everything else the core sent, preserved.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl ProxyView {
    /// The proxy or group this one dials *through*, when it names one.
    ///
    /// The API reports the field — it is empty for a proxy that dials directly
    /// — and the core validates the chain itself: a name that is neither a
    /// proxy nor a group is refused, and so is any cycle.
    #[must_use]
    pub fn dialer_proxy(&self) -> Option<&str> {
        self.extra
            .get("dialer-proxy")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
    }

    /// `true` when this entry is a policy group rather than a node.
    #[must_use]
    pub fn is_group(&self) -> bool {
        self.all.is_some()
    }

    /// Members, or an empty slice for a plain proxy.
    #[must_use]
    pub fn members(&self) -> &[String] {
        self.all.as_deref().unwrap_or(&[])
    }

    /// The most recent latency reading, if any.
    #[must_use]
    pub fn latest_delay(&self) -> Option<u16> {
        self.history.last().map(|s| s.delay)
    }

    /// The most recent reading, treating `0` as "not measured" the way the core
    /// and every dashboard do.
    #[must_use]
    pub fn latest_delay_or_none(&self) -> Option<u16> {
        self.latest_delay().filter(|d| *d > 0)
    }

    /// Protocol or group type, lower-cased for display.
    #[must_use]
    pub fn kind_lower(&self) -> String {
        self.kind.to_ascii_lowercase()
    }

    /// `true` when a selection can be pinned with `PUT /proxies/{name}`.
    ///
    /// Only `Selector`, `URLTest` and `Fallback` implement selection;
    /// `LoadBalance` and plain proxies reject it with `400 Must be a Selector`.
    #[must_use]
    pub fn is_selectable(&self) -> bool {
        matches!(self.kind.as_str(), "Selector" | "URLTest" | "Fallback")
    }

    /// `true` when `DELETE /proxies/{name}` can clear a pinned member.
    #[must_use]
    pub fn has_fixed_selection(&self) -> bool {
        matches!(self.kind.as_str(), "URLTest" | "Fallback")
    }
}

/// `GET /proxies`
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProxiesResponse {
    /// Flat, name-keyed map. The core returns it in Go map order, so callers
    /// must sort before display.
    #[serde(default, deserialize_with = "null_as_default")]
    pub proxies: BTreeMap<String, ProxyView>,
}

/// `GET /group`
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GroupsResponse {
    /// Policy groups only.
    #[serde(default, deserialize_with = "null_as_default")]
    pub proxies: Vec<ProxyView>,
}

/// One entry of `GET /providers/proxies`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProxyProviderInfo {
    /// Provider name.
    #[serde(default)]
    pub name: String,
    /// Always `Proxy`.
    #[serde(rename = "type", default)]
    pub kind: String,
    /// `File`, `HTTP`, `Inline` or `Compatible`.
    #[serde(rename = "vehicleType", default)]
    pub vehicle_type: String,
    /// The provider's nodes.
    #[serde(default, deserialize_with = "null_as_default")]
    pub proxies: Vec<ProxyView>,
    /// Health-check URL.
    #[serde(rename = "testUrl", default, skip_serializing_if = "Option::is_none")]
    pub test_url: Option<String>,
    /// Accepted status expression, `*` when unset.
    #[serde(
        rename = "expectedStatus",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub expected_status: Option<String>,
    /// Refresh time. Emitted as the zero time for providers that never update.
    #[serde(rename = "updatedAt", default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// Extra keys.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl ProxyProviderInfo {
    /// `true` for the synthetic providers mihomo invents for inline config.
    ///
    /// `GET /providers/proxies` always includes one called `default` holding the
    /// top-level `proxies:` list, plus one per group that declares inline
    /// members. They cannot be refreshed meaningfully.
    #[must_use]
    pub fn is_synthetic(&self) -> bool {
        self.vehicle_type == "Compatible" || self.vehicle_type == "Inline"
    }

    /// `true` when `updatedAt` is the Go zero time, i.e. never refreshed.
    #[must_use]
    pub fn never_updated(&self) -> bool {
        self.updated_at
            .as_deref()
            .is_none_or(|s| s.starts_with("0001-01-01"))
    }
}

/// `GET /providers/proxies`
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProxyProvidersResponse {
    /// Name-keyed providers.
    #[serde(default, deserialize_with = "null_as_default")]
    pub providers: BTreeMap<String, ProxyProviderInfo>,
}

/// One entry of `GET /providers/rules`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuleProviderInfo {
    /// `Domain`, `IPCIDR` or `Classical`.
    #[serde(default)]
    pub behavior: String,
    /// `YamlRule`, `TextRule` or `MrsRule`; empty for inline providers.
    #[serde(default)]
    pub format: String,
    /// Provider name.
    #[serde(default)]
    pub name: String,
    /// Number of rules in the set.
    #[serde(rename = "ruleCount", default)]
    pub rule_count: u64,
    /// Always `Rule`.
    #[serde(rename = "type", default)]
    pub kind: String,
    /// `File`, `HTTP`, `Inline` or `Compatible`.
    #[serde(rename = "vehicleType", default)]
    pub vehicle_type: String,
    /// Last refresh time.
    #[serde(rename = "updatedAt", default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// Inline payload, present only for inline providers.
    #[serde(
        default,
        deserialize_with = "null_as_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub payload: Vec<String>,
    /// Extra keys.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `GET /providers/rules`
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RuleProvidersResponse {
    /// Name-keyed providers.
    #[serde(default, deserialize_with = "null_as_default")]
    pub providers: BTreeMap<String, RuleProviderInfo>,
}

/// Per-rule counters, present only when the core wraps rules for statistics.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleStats {
    /// Whether the rule is currently disabled.
    #[serde(default)]
    pub disabled: bool,
    /// Times the rule matched.
    #[serde(rename = "hitCount", default)]
    pub hit_count: u64,
    /// When it last matched.
    #[serde(rename = "hitAt", default)]
    pub hit_at: String,
    /// Times it was evaluated and did not match.
    #[serde(rename = "missCount", default)]
    pub miss_count: u64,
    /// When it was last evaluated without matching.
    #[serde(rename = "missAt", default)]
    pub miss_at: String,
}

impl RuleStats {
    /// Total evaluations.
    #[must_use]
    pub fn evaluations(&self) -> u64 {
        self.hit_count.saturating_add(self.miss_count)
    }

    /// `true` when the rule has never fired but has been evaluated plenty.
    #[must_use]
    pub fn looks_dead(&self, min_evaluations: u64) -> bool {
        !self.disabled && self.hit_count == 0 && self.evaluations() >= min_evaluations
    }
}

/// One entry of `GET /rules`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuleInfo {
    /// Position in the rule list; used as the key for `PATCH /rules/disable`.
    #[serde(default)]
    pub index: u32,
    /// Rule type in PascalCase, e.g. `DomainSuffix`, `RuleSet`, `Match`.
    #[serde(rename = "type", default)]
    pub kind: String,
    /// The value the rule matches.
    #[serde(default)]
    pub payload: String,
    /// Target policy; empty for rules with no target.
    #[serde(default)]
    pub proxy: String,
    /// Entry count for `GeoIP`/`GeoSite`, `-1` otherwise.
    #[serde(default)]
    pub size: i64,
    /// Counters; absent when the core is not tracking them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<RuleStats>,
    /// Extra keys.
    #[serde(flatten)]
    pub other: Map<String, Value>,
}

impl RuleInfo {
    /// The rule as it would be written in a configuration file, e.g.
    /// `DOMAIN-SUFFIX,example.com,PROXY`.
    ///
    /// Bridges the two spellings the core uses: PascalCase over the API,
    /// upper-kebab in config files.
    #[must_use]
    pub fn as_config_rule(&self) -> String {
        let kind = rule_kind_to_config(&self.kind);
        if self.payload.is_empty() || self.kind == "Match" {
            format!("{kind},{}", self.proxy)
        } else {
            format!("{kind},{},{}", self.payload, self.proxy)
        }
    }

    /// Counters, or a default when the core did not send any.
    #[must_use]
    pub fn stats(&self) -> RuleStats {
        self.extra.clone().unwrap_or_default()
    }
}

/// `GET /rules`
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RulesResponse {
    /// The rule list, in evaluation order.
    #[serde(default, deserialize_with = "null_as_default")]
    pub rules: Vec<RuleInfo>,
}

/// Canonical mapping between the core's PascalCase rule types and the
/// upper-kebab spelling used in configuration files.
///
/// One table drives both directions. Deriving the reverse mapping mechanically
/// would be lossy — `GeoSite` would come back as `Geosite` and `IPCIDR` as
/// `Ipcidr` — so every irregular name is enumerated here.
pub const RULE_KINDS: &[(&str, &str)] = &[
    ("Domain", "DOMAIN"),
    ("DomainSuffix", "DOMAIN-SUFFIX"),
    ("DomainKeyword", "DOMAIN-KEYWORD"),
    ("DomainRegex", "DOMAIN-REGEX"),
    ("DomainWildcard", "DOMAIN-WILDCARD"),
    ("GeoSite", "GEOSITE"),
    ("GeoIP", "GEOIP"),
    ("SrcGeoIP", "SRC-GEOIP"),
    ("IPASN", "IP-ASN"),
    ("SrcIPASN", "SRC-IP-ASN"),
    ("IPCIDR", "IP-CIDR"),
    ("SrcIPCIDR", "SRC-IP-CIDR"),
    ("IPSuffix", "IP-SUFFIX"),
    ("SrcIPSuffix", "SRC-IP-SUFFIX"),
    ("SrcPort", "SRC-PORT"),
    ("DstPort", "DST-PORT"),
    ("InPort", "IN-PORT"),
    ("InUser", "IN-USER"),
    ("InName", "IN-NAME"),
    ("InType", "IN-TYPE"),
    ("ProcessName", "PROCESS-NAME"),
    ("ProcessPath", "PROCESS-PATH"),
    ("ProcessNameRegex", "PROCESS-NAME-REGEX"),
    ("ProcessPathRegex", "PROCESS-PATH-REGEX"),
    ("ProcessNameWildcard", "PROCESS-NAME-WILDCARD"),
    ("ProcessPathWildcard", "PROCESS-PATH-WILDCARD"),
    ("RematchName", "REMATCH-NAME"),
    ("Match", "MATCH"),
    ("RuleSet", "RULE-SET"),
    ("SubRules", "SUB-RULE"),
    ("Network", "NETWORK"),
    ("DSCP", "DSCP"),
    ("Uid", "UID"),
    ("AND", "AND"),
    ("OR", "OR"),
    ("NOT", "NOT"),
];

/// Rule types the core accepts in a *document*, as the document spells them.
///
/// Not the same list as [`RULE_KINDS`], which is a translation table. The core
/// reports both `IP-CIDR` and `IP-CIDR6` as the `IPCIDR` adapter — verified by
/// starting a core with one of each and reading `GET /rules`: two `IPCIDR`
/// entries — so `IP-CIDR6` has no row of its own, and a check built on the
/// table reported a rule type the core loads (which `mihomo -t` confirms) as
/// one it does not know.
pub fn is_config_rule_kind(kind: &str) -> bool {
    const CONFIG_ONLY: &[&str] = &["IP-CIDR6"];
    CONFIG_ONLY.contains(&kind) || RULE_KINDS.iter().any(|(_, config)| *config == kind)
}

/// Translate a PascalCase rule type from the API into config spelling.
///
/// Unknown types are still converted, by splitting on case boundaries, so a
/// rule added by a future core version renders sensibly instead of being
/// dropped.
#[must_use]
pub fn rule_kind_to_config(kind: &str) -> String {
    if let Some((_, config)) = RULE_KINDS.iter().find(|(api, _)| *api == kind) {
        return (*config).to_owned();
    }
    let mut out = String::with_capacity(kind.len() + 4);
    for (i, ch) in kind.chars().enumerate() {
        if ch.is_ascii_uppercase() && i > 0 {
            out.push('-');
        }
        out.push(ch.to_ascii_uppercase());
    }
    out
}

/// Translate a config-spelling rule type into the API's PascalCase form.
#[must_use]
pub fn rule_kind_to_api(kind: &str) -> String {
    let upper = kind.to_ascii_uppercase();
    if let Some((api, _)) = RULE_KINDS.iter().find(|(_, config)| *config == upper) {
        return (*api).to_owned();
    }
    if upper == "FINAL" {
        return "Match".to_owned();
    }
    upper
        .split('-')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => {
                    first.to_ascii_uppercase().to_string() + &chars.as_str().to_ascii_lowercase()
                }
                None => String::new(),
            }
        })
        .collect()
}

/// Connection metadata.
///
/// Ports are **strings** on the wire because the core's struct tags carry
/// `,string`; [`Self::source_port`] and friends parse them for convenience.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Metadata {
    /// `tcp`, `udp` or `all`.
    #[serde(default)]
    pub network: String,
    /// Inbound type, e.g. `HTTP`, `Tun`, `Socks5`.
    #[serde(rename = "type", default)]
    pub kind: String,
    /// Client address.
    #[serde(rename = "sourceIP", default)]
    pub source_ip: String,
    /// Resolved destination address.
    #[serde(rename = "destinationIP", default)]
    pub destination_ip: String,
    /// Client port, as a JSON string or a number.
    ///
    /// The core sends a string, because Go's encoder was handed one — but a
    /// port is a number, and a client that accepts only the string spelling
    /// breaks the first time something in front of the core rewrites the
    /// response.
    #[serde(rename = "sourcePort", default, deserialize_with = "string_or_number")]
    pub source_port: String,
    /// Destination port, as a JSON string or a number.
    #[serde(
        rename = "destinationPort",
        default,
        deserialize_with = "string_or_number"
    )]
    pub destination_port: String,
    /// Hostname from the request, when known.
    #[serde(default)]
    pub host: String,
    /// Host recovered by sniffing, when sniffing is on.
    #[serde(rename = "sniffHost", default)]
    pub sniff_host: String,
    /// Resolver mode in effect: `normal` or `fake-ip`.
    #[serde(rename = "dnsMode", default)]
    pub dns_mode: String,
    /// Owning process name, when process matching is enabled.
    #[serde(default)]
    pub process: String,
    /// Owning process path.
    #[serde(rename = "processPath", default)]
    pub process_path: String,
    /// Rule that rematched this connection.
    #[serde(rename = "rematchName", default)]
    pub rematch_name: String,
    /// The address the client actually dialled, before redirection.
    #[serde(rename = "remoteDestination", default)]
    pub remote_destination: String,
    /// Inbound listener name.
    #[serde(rename = "inboundName", default)]
    pub inbound_name: String,
    /// Extra keys.
    #[serde(flatten)]
    pub other: Map<String, Value>,
}

impl Metadata {
    /// Destination port as a number, if it parsed.
    #[must_use]
    pub fn destination_port_num(&self) -> Option<u16> {
        self.destination_port.parse().ok()
    }

    /// Source port as a number, if it parsed.
    #[must_use]
    pub fn source_port_num(&self) -> Option<u16> {
        self.source_port.parse().ok()
    }

    /// The most specific name for the destination: sniffed host, then the
    /// request's `Host` header, then the IP.
    #[must_use]
    pub fn destination_label(&self) -> String {
        for candidate in [&self.sniff_host, &self.host, &self.destination_ip] {
            if !candidate.is_empty() {
                return candidate.clone();
            }
        }
        "-".to_owned()
    }

    /// `host:port` for display, using the numeric destination port when known.
    #[must_use]
    pub fn destination_endpoint(&self) -> String {
        let label = self.destination_label();
        match self.destination_port_num() {
            Some(p) => format!("{label}:{p}"),
            None if self.destination_port.is_empty() => label,
            None => format!("{label}:{}", self.destination_port),
        }
    }
}

/// One live connection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Connection {
    /// UUID used to close this connection individually.
    pub id: String,
    /// Connection metadata; can be `null` for some UDP paths.
    #[serde(default)]
    pub metadata: Option<Metadata>,
    /// Bytes sent by the client.
    #[serde(default)]
    pub upload: u64,
    /// Bytes received from the remote.
    #[serde(default)]
    pub download: u64,
    /// RFC3339Nano start time with a local offset.
    #[serde(default)]
    pub start: String,
    /// Proxy chain, **outbound first**: element 0 dialled, the last element is
    /// the outermost group the rule selected.
    #[serde(default, deserialize_with = "null_as_default")]
    pub chains: Vec<String>,
    /// Provider chain, index-aligned with `chains`, empty where not applicable.
    #[serde(
        rename = "providerChains",
        default,
        deserialize_with = "null_as_default"
    )]
    pub provider_chains: Vec<String>,
    /// Matched rule type in PascalCase, empty when nothing matched.
    #[serde(default)]
    pub rule: String,
    /// Matched rule payload.
    #[serde(rename = "rulePayload", default)]
    pub rule_payload: String,
    /// Extra keys.
    #[serde(flatten)]
    pub other: Map<String, Value>,
}

impl Connection {
    /// Total bytes, both directions.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.upload.saturating_add(self.download)
    }

    /// Metadata, or an empty default so callers never have to unwrap.
    #[must_use]
    pub fn meta(&self) -> Metadata {
        self.metadata.clone().unwrap_or_default()
    }

    /// The outermost group that produced this connection, i.e. the group the
    /// rule selected.
    #[must_use]
    pub fn selected_group(&self) -> Option<&str> {
        self.chains
            .last()
            .map(String::as_str)
            .filter(|s| !s.is_empty())
    }

    /// The node that actually dialled: the first chain element that is not a
    /// built-in target.
    #[must_use]
    pub fn outbound_node(&self) -> Option<&str> {
        self.chains
            .first()
            .map(String::as_str)
            .filter(|s| !s.is_empty() && !is_builtin_target(s))
    }

    /// The matched rule rendered the way it appears in a config file.
    #[must_use]
    pub fn rule_label(&self) -> String {
        if self.rule.is_empty() {
            return "-".to_owned();
        }
        let kind = rule_kind_to_config(&self.rule);
        if self.rule_payload.is_empty() {
            kind
        } else {
            format!("{kind},{}", self.rule_payload)
        }
    }
}

/// `true` for the built-in policies that are not real nodes.
#[must_use]
pub fn is_builtin_target(name: &str) -> bool {
    matches!(
        name.to_ascii_uppercase().as_str(),
        "DIRECT" | "REJECT" | "REJECT-DROP" | "PASS" | "PASS-RULE" | "COMPATIBLE" | "GLOBAL"
    )
}

/// `GET /connections`
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ConnectionsResponse {
    /// Cumulative bytes downloaded since the core started. Survives
    /// `DELETE /connections`.
    #[serde(rename = "downloadTotal", default)]
    pub download_total: u64,
    /// Cumulative bytes uploaded since the core started.
    #[serde(rename = "uploadTotal", default)]
    pub upload_total: u64,
    /// The connections, or `None` when the core sent `null` for "none".
    #[serde(default)]
    pub connections: Option<Vec<Connection>>,
    /// Resident memory in bytes; may be `0` immediately after start.
    #[serde(default)]
    pub memory: u64,
}

impl ConnectionsResponse {
    /// Connections as a slice, never `None`.
    #[must_use]
    pub fn items(&self) -> &[Connection] {
        self.connections.as_deref().unwrap_or(&[])
    }

    /// Sum of every connection's traffic.
    #[must_use]
    pub fn total_bytes(&self) -> u64 {
        self.upload_total.saturating_add(self.download_total)
    }
}

/// `GET /traffic`
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Traffic {
    /// Bytes per second uploaded in the last interval.
    #[serde(default)]
    pub up: u64,
    /// Bytes per second downloaded in the last interval.
    #[serde(default)]
    pub down: u64,
    /// Cumulative bytes uploaded; absent on older cores.
    #[serde(rename = "upTotal", default)]
    pub up_total: u64,
    /// Cumulative bytes downloaded; absent on older cores.
    #[serde(rename = "downTotal", default)]
    pub down_total: u64,
}

impl Traffic {
    /// Instantaneous throughput, both directions.
    #[must_use]
    pub fn rate(&self) -> u64 {
        self.up.saturating_add(self.down)
    }
}

/// `GET /memory`
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Memory {
    /// Resident set size in bytes. The **first** frame is always `0`.
    #[serde(default)]
    pub inuse: u64,
    /// Always `0` in current cores; kept for compatibility.
    #[serde(default)]
    pub oslimit: u64,
}

/// Log levels the core accepts, quietest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    /// Nothing is emitted.
    Silent,
    /// Errors only.
    Error,
    /// Errors and warnings.
    Warning,
    /// The default.
    Info,
    /// Everything, including per-connection detail.
    Debug,
}

impl LogLevel {
    /// Parse the core's spelling. Note `warn` is **not** accepted by the core;
    /// `warning` is.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "silent" => Some(Self::Silent),
            "error" => Some(Self::Error),
            "warning" => Some(Self::Warning),
            "info" => Some(Self::Info),
            "debug" => Some(Self::Debug),
            _ => None,
        }
    }

    /// The string the core expects, for log filtering and `PATCH /configs`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Silent => "silent",
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Info => "info",
            Self::Debug => "debug",
        }
    }

    /// Every level, quietest first.
    #[must_use]
    pub fn all() -> [Self; 5] {
        [
            Self::Silent,
            Self::Error,
            Self::Warning,
            Self::Info,
            Self::Debug,
        ]
    }
}

/// One frame from `GET /logs`.
///
/// The envelope is flat — `type` carries the level and `payload` the text —
/// with no `{"type":"log"}` wrapper.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogEvent {
    /// Severity, as the core's level string.
    #[serde(rename = "type", default)]
    pub level: String,
    /// The log line.
    #[serde(default)]
    pub payload: String,
}

impl LogEvent {
    /// The level as an enum, if recognised.
    #[must_use]
    pub fn level(&self) -> Option<LogLevel> {
        LogLevel::parse(&self.level)
    }

    /// Case-insensitive substring search over the message.
    #[must_use]
    pub fn matches(&self, needle: &str) -> bool {
        if needle.is_empty() {
            return true;
        }
        self.payload
            .to_ascii_lowercase()
            .contains(&needle.to_ascii_lowercase())
    }
}

/// `GET /proxies/{name}/delay`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelayResponse {
    /// Measured latency in milliseconds.
    pub delay: u16,
}

/// `POST /restart` and `POST /upgrade*`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusOk {
    /// Always `ok` on success.
    pub status: String,
}

/// The error envelope every API failure uses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiMessage {
    /// Human-readable reason.
    #[serde(default)]
    pub message: String,
}

/// Map of member name to delay, from `GET /group/{name}/delay`.
///
/// Members that failed are simply absent, and `0` is a legitimate value here
/// even though the single-proxy endpoint rejects it.
pub type GroupDelay = BTreeMap<String, u16>;

/// The live general settings from `GET /configs`.
///
/// Only the keys worth showing in a settings pane are typed; the rest are kept
/// so a `PATCH` can round-trip without losing them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GeneralConfig {
    /// HTTP proxy port.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// SOCKS5 port.
    #[serde(
        rename = "socks-port",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub socks_port: Option<u16>,
    /// Mixed HTTP+SOCKS port.
    #[serde(
        rename = "mixed-port",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub mixed_port: Option<u16>,
    /// `rule`, `global` or `direct`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Log level.
    #[serde(rename = "log-level", default, skip_serializing_if = "Option::is_none")]
    pub log_level: Option<String>,
    /// Whether IPv6 is enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ipv6: Option<bool>,
    /// Whether the proxy listens on all interfaces.
    #[serde(rename = "allow-lan", default, skip_serializing_if = "Option::is_none")]
    pub allow_lan: Option<bool>,
    /// Bind address for the proxy listeners.
    #[serde(
        rename = "bind-address",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub bind_address: Option<String>,
    /// Normalise latency by subtracting the handshake time.
    #[serde(
        rename = "unified-delay",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub unified_delay: Option<bool>,
    /// Open TCP connections to all resolved addresses concurrently.
    #[serde(
        rename = "tcp-concurrent",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub tcp_concurrent: Option<bool>,
    /// Process lookup strategy: `always`, `strict` or `off`.
    #[serde(
        rename = "find-process-mode",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub find_process_mode: Option<String>,
    /// TUN listener settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tun: Option<Map<String, Value>>,
    /// Everything else, so a patch round-trips losslessly.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// The subset of `GeneralConfig` that `PATCH /configs` accepts.
///
/// Fields left as `None` are not sent and therefore not touched, because the
/// core models them as pointers.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ConfigPatch {
    /// `rule`, `global` or `direct`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Log level.
    #[serde(rename = "log-level", default, skip_serializing_if = "Option::is_none")]
    pub log_level: Option<String>,
    /// Enable or disable IPv6.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ipv6: Option<bool>,
    /// Whether to listen on all interfaces.
    #[serde(rename = "allow-lan", default, skip_serializing_if = "Option::is_none")]
    pub allow_lan: Option<bool>,
    /// Mixed listener port.
    #[serde(
        rename = "mixed-port",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub mixed_port: Option<u16>,
    /// Enable or disable TUN.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tun: Option<Map<String, Value>>,
    /// Normalise latency measurement.
    #[serde(
        rename = "unified-delay",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub unified_delay: Option<bool>,
    /// Concurrent TCP dialling.
    #[serde(
        rename = "tcp-concurrent",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub tcp_concurrent: Option<bool>,
}

impl ConfigPatch {
    /// `true` when nothing would be sent.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.mode.is_none()
            && self.log_level.is_none()
            && self.ipv6.is_none()
            && self.allow_lan.is_none()
            && self.mixed_port.is_none()
            && self.tun.is_none()
            && self.unified_delay.is_none()
            && self.tcp_concurrent.is_none()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_real_version_response() {
        let v: Version = serde_json::from_str(r#"{"meta":true,"version":"v1.19.31"}"#).unwrap();
        assert!(v.meta);
        assert_eq!(v.trimmed(), "1.19.31");
        assert_eq!(v.semver(), Some((1, 19, 31)));

        // The old Dreamacro field must not be required.
        let old: Version = serde_json::from_str(r#"{"version":"1.10.0"}"#).unwrap();
        assert!(!old.meta);
        assert_eq!(old.semver(), Some((1, 10, 0)));
    }

    #[test]
    fn parses_a_real_direct_proxy() {
        let raw = r#"{"alive":true,"dialer-proxy":"","extra":{},"history":[],
            "id":"fae47b09-9950-4ee3-be48-fb04f83906d7","interface":"","mptcp":false,
            "name":"DIRECT","provider-name":"","routing-mark":0,"smux":false,"tfo":false,
            "type":"Direct","udp":true,"uot":false,"xudp":false}"#;
        let p: ProxyView = serde_json::from_str(raw).unwrap();
        assert_eq!(p.name, "DIRECT");
        assert_eq!(p.kind, "Direct");
        assert!(!p.is_group(), "no `all` means not a group");
        assert!(p.members().is_empty());
        assert!(p.latest_delay().is_none());
        assert!(!p.is_selectable());
        assert!(p.extra.contains_key("xudp"), "unknown keys are preserved");
    }

    #[test]
    fn parses_a_real_selector_group_and_marks_it_selectable() {
        let raw = r#"{"alive":true,"all":["node-a","node-b","DIRECT"],"dialer-proxy":"",
            "emptyFallback":"COMPATIBLE","extra":{},"hidden":false,"history":[],"icon":"",
            "interface":"","mptcp":false,"name":"grp-select","now":"node-a","provider-name":"",
            "routing-mark":0,"smux":false,"testUrl":"","tfo":false,"type":"Selector",
            "udp":false,"uot":false,"xudp":false}"#;
        let g: ProxyView = serde_json::from_str(raw).unwrap();
        assert!(g.is_group());
        assert_eq!(g.members(), ["node-a", "node-b", "DIRECT"]);
        assert_eq!(g.now.as_deref(), Some("node-a"));
        assert!(g.is_selectable());
        assert!(!g.has_fixed_selection(), "URLTest/Fallback only");
        assert!(g.id.is_none(), "groups carry no id");
    }

    #[test]
    fn recognises_a_pinned_url_test_group() {
        let raw =
            r#"{"all":["a"],"name":"auto","type":"URLTest","fixed":"a","expectedStatus":"*"}"#;
        let g: ProxyView = serde_json::from_str(raw).unwrap();
        assert!(g.is_selectable());
        assert!(g.has_fixed_selection());
        assert_eq!(g.fixed.as_deref(), Some("a"));
    }

    #[test]
    fn parses_connections_including_the_null_empty_case() {
        let empty: ConnectionsResponse = serde_json::from_str(
            r#"{"downloadTotal":0,"uploadTotal":0,"connections":null,"memory":0}"#,
        )
        .unwrap();
        assert!(
            empty.items().is_empty(),
            "null means no connections, not a parse error"
        );

        let raw = r#"{"downloadTotal":0,"uploadTotal":87,
          "connections":[{"id":"14ddd52c-5d8c-4ece-afd3-7125c0637db4",
            "metadata":{"network":"tcp","type":"HTTP","sourceIP":"127.0.0.1",
              "destinationIP":"127.0.0.1","sourcePort":"50130","destinationPort":"18099",
              "inboundName":"DEFAULT-MIXED","host":"","dnsMode":"normal","process":"",
              "sniffHost":"example.com"},
            "upload":87,"download":0,"start":"2026-09-25T17:40:55.786450952+08:00",
            "chains":["DIRECT","grp-select"],"providerChains":["",""],
            "rule":"Match","rulePayload":""}],"memory":0}"#;
        let c: ConnectionsResponse = serde_json::from_str(raw).unwrap();
        let conn = &c.items()[0];
        assert_eq!(conn.total(), 87);
        // Chains are outbound-first: the last element is the selected group.
        assert_eq!(conn.selected_group(), Some("grp-select"));
        assert_eq!(conn.outbound_node(), None, "DIRECT is not a node");
        assert_eq!(conn.rule_label(), "MATCH");
        let m = conn.meta();
        assert_eq!(m.destination_port_num(), Some(18099));
        assert_eq!(
            m.destination_endpoint(),
            "example.com:18099",
            "sniffed host wins"
        );
    }

    #[test]
    fn picks_the_real_node_out_of_a_chain() {
        let raw = r#"{"id":"x","chains":["JP 01","PROXY","GLOBAL"],"metadata":null,
            "rule":"DomainSuffix","rulePayload":"google.com"}"#;
        let c: Connection = serde_json::from_str(raw).unwrap();
        assert_eq!(c.outbound_node(), Some("JP 01"));
        assert_eq!(c.selected_group(), Some("GLOBAL"));
        assert_eq!(c.rule_label(), "DOMAIN-SUFFIX,google.com");
        assert_eq!(c.meta().destination_label(), "-");
    }

    #[test]
    fn metadata_falls_back_through_host_then_ip() {
        let mut m = Metadata {
            destination_ip: "1.2.3.4".into(),
            destination_port: "443".into(),
            ..Metadata::default()
        };
        assert_eq!(m.destination_endpoint(), "1.2.3.4:443");
        m.host = "example.com".into();
        assert_eq!(m.destination_endpoint(), "example.com:443");
        m.sniff_host = "sniffed.example".into();
        assert_eq!(m.destination_endpoint(), "sniffed.example:443");
    }

    #[test]
    fn parses_flat_log_frames() {
        let e: LogEvent =
            serde_json::from_str(r#"{"type":"warning","payload":"[TCP] dial failed"}"#).unwrap();
        assert_eq!(e.level(), Some(LogLevel::Warning));
        assert!(e.matches("DIAL"), "search is case-insensitive");
        assert!(e.matches(""));
        assert!(!e.matches("nothing"));

        // `warn` is not a level the core ever emits; `warning` is.
        assert_eq!(LogLevel::parse("warn"), None);
        assert_eq!(LogLevel::parse("warning"), Some(LogLevel::Warning));
        assert_eq!(LogLevel::parse("INFO"), Some(LogLevel::Info));
    }

    #[test]
    fn parses_traffic_with_and_without_totals() {
        let t: Traffic =
            serde_json::from_str(r#"{"up":1,"down":2,"upTotal":3,"downTotal":4}"#).unwrap();
        assert_eq!((t.up, t.down, t.rate(), t.up_total), (1, 2, 3, 3));
        let old: Traffic = serde_json::from_str(r#"{"up":1,"down":2}"#).unwrap();
        assert_eq!(old.up_total, 0, "older cores omit the totals");
    }

    #[test]
    fn parses_rules_and_translates_both_spellings() {
        let raw = r#"{"rules":[{"index":0,"type":"DomainSuffix","payload":"example.com",
            "proxy":"grp-select","size":-1,"extra":{"disabled":false,"hitCount":0,
            "hitAt":"1970-01-01T07:30:00+07:30","missCount":0,"missAt":"1970-01-01T07:30:00+07:30"}},
            {"index":1,"type":"Match","payload":"","proxy":"DIRECT","size":-1}]}"#;
        let r: RulesResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(
            r.rules[0].as_config_rule(),
            "DOMAIN-SUFFIX,example.com,grp-select"
        );
        assert_eq!(r.rules[1].as_config_rule(), "MATCH,DIRECT");
        let stats = r.rules[0].stats();
        assert!(!stats.disabled);
        assert_eq!(stats.evaluations(), 0);
        assert!(
            !stats.looks_dead(1),
            "a rule with no evaluations is not yet dead"
        );
    }

    #[test]
    fn detects_a_rule_that_never_fires() {
        let s = RuleStats {
            hit_count: 0,
            miss_count: 10_000,
            ..RuleStats::default()
        };
        assert!(s.looks_dead(1000));
        assert!(!s.looks_dead(100_001));
        let disabled = RuleStats {
            disabled: true,
            miss_count: 10_000,
            ..RuleStats::default()
        };
        assert!(
            !disabled.looks_dead(1),
            "a deliberately disabled rule is not 'dead'"
        );
    }

    #[test]
    fn rule_kind_translation_round_trips_for_every_known_kind() {
        for (api, config) in RULE_KINDS {
            assert_eq!(&rule_kind_to_config(api), config, "api -> config for {api}");
            assert_eq!(&rule_kind_to_api(config), api, "config -> api for {config}");
        }
    }

    #[test]
    fn rule_kind_translation_handles_irregular_capitalisation() {
        // These are exactly the names a mechanical case-splitting conversion
        // gets wrong, which is why they are enumerated in RULE_KINDS.
        assert_eq!(rule_kind_to_config("GeoSite"), "GEOSITE");
        assert_eq!(rule_kind_to_api("GEOSITE"), "GeoSite");
        assert_eq!(rule_kind_to_config("GeoIP"), "GEOIP");
        assert_eq!(rule_kind_to_api("GEOIP"), "GeoIP");
        assert_eq!(rule_kind_to_config("IPCIDR"), "IP-CIDR");
        assert_eq!(rule_kind_to_api("IP-CIDR"), "IPCIDR");
        assert_eq!(
            rule_kind_to_api("IP-CIDR6"),
            "IpCidr6",
            "unknown kinds degrade gracefully"
        );
        assert_eq!(
            rule_kind_to_api("FINAL"),
            "Match",
            "FINAL is an accepted alias"
        );
    }

    #[test]
    fn parses_rule_and_proxy_providers() {
        let rules: RuleProvidersResponse = serde_json::from_str(
            r#"{"providers":{"rp-inline":{"behavior":"Domain","format":"","name":"rp-inline",
                "ruleCount":2,"type":"Rule","vehicleType":"Inline",
                "updatedAt":"2026-09-25T17:33:42.768369569+08:00",
                "payload":["example.com","example.org"]}}}"#,
        )
        .unwrap();
        let rp = rules.providers.get("rp-inline").unwrap();
        assert_eq!(rp.behavior, "Domain");
        assert_eq!(rp.rule_count, 2);
        assert_eq!(rp.payload.len(), 2);

        let proxies: ProxyProvidersResponse = serde_json::from_str(
            r#"{"providers":{"default":{"name":"default","type":"Proxy","vehicleType":"Compatible",
                "proxies":[],"testUrl":"","expectedStatus":"*",
                "updatedAt":"0001-01-01T00:00:00Z"}}}"#,
        )
        .unwrap();
        let pp = proxies.providers.get("default").unwrap();
        assert!(pp.is_synthetic(), "inline providers cannot be refreshed");
        assert!(pp.never_updated(), "the zero time means never updated");
    }

    #[test]
    fn config_patch_omits_untouched_fields() {
        let patch = ConfigPatch {
            mode: Some("global".into()),
            ..ConfigPatch::default()
        };
        assert!(!patch.is_empty());
        let json = serde_json::to_value(&patch).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"mode": "global"}),
            "omitted fields stay untouched"
        );
        assert!(ConfigPatch::default().is_empty());
    }

    #[test]
    fn general_config_round_trips_unknown_keys() {
        let raw = r#"{"port":0,"mixed-port":17890,"mode":"rule","log-level":"debug",
            "ipv6":false,"allow-lan":false,"bind-address":"*","unified-delay":false,
            "tcp-concurrent":false,"find-process-mode":"strict",
            "tun":{"enable":false,"stack":"gVisor"},"etag-support":true}"#;
        let g: GeneralConfig = serde_json::from_str(raw).unwrap();
        assert_eq!(g.mixed_port, Some(17890));
        assert_eq!(g.log_level.as_deref(), Some("debug"));
        assert!(g.extra.contains_key("etag-support"));
        let round = serde_json::to_value(&g).unwrap();
        assert_eq!(round["etag-support"], serde_json::json!(true));
        assert_eq!(round["tun"]["stack"], serde_json::json!("gVisor"));
    }

    #[test]
    fn group_delay_accepts_zero_and_omits_failures() {
        let d: GroupDelay = serde_json::from_str(r#"{"DIRECT":0,"node-a":42}"#).unwrap();
        assert_eq!(d.get("DIRECT"), Some(&0), "0 is a valid group delay");
        assert_eq!(d.get("node-b"), None);
    }
}
