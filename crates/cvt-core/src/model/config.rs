//! The runtime configuration document.
//!
//! # Why this is not a big struct
//!
//! A mihomo config has well over a hundred top-level keys and its shape varies
//! with the core version, the subscription, and the user's overrides. A
//! `struct` with named fields would:
//!
//! * silently drop every key it does not know about, corrupting subscriptions,
//! * need a code change for each new mihomo release, and
//! * make deep merging awkward, because serde's `flatten` loses the original
//!   ordering and cannot represent duplicate keys.
//!
//! So [`Config`] is a thin, *always lossless* wrapper around an ordered JSON
//! object. Merging and path patching operate on that object; typed accessors
//! ([`Config::proxies`], [`Config::rules`], [`Config::dns`], ...) parse just the
//! subtree they need, on demand, and never write back implicitly.
//!
//! See `docs/adr/0002-lossless-config-document.md` for the full rationale.

use serde_json::{Map, Value, json};

use crate::error::{Error, Result};
use crate::model::proxy::{Proxy, ProxyGroup, ProxyProvider};
use crate::model::rule::Rule;

/// Working modes mihomo understands.
pub const MODES: &[&str] = &["rule", "global", "direct"];

/// Log levels mihomo understands, quietest first.
pub const LOG_LEVELS: &[&str] = &["silent", "error", "warning", "info", "debug"];

/// A complete mihomo configuration, preserved byte-for-byte on round-trip.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Config {
    root: Map<String, Value>,
}

impl Config {
    /// An empty configuration.
    #[must_use]
    pub fn empty() -> Self {
        Self { root: Map::new() }
    }

    /// Wrap an existing mapping.
    #[must_use]
    pub fn from_map(root: Map<String, Value>) -> Self {
        Self { root }
    }

    /// Parse from a JSON value that must be an object.
    ///
    /// # Errors
    /// [`Error::InvalidValue`] when the value is not an object.
    pub fn from_value(value: Value) -> Result<Self> {
        match value {
            Value::Object(m) => Ok(Self { root: m }),
            Value::Null => Ok(Self::empty()),
            other => Err(Error::invalid(
                "config",
                format!(
                    "expected a mapping at the document root, found {}",
                    type_name(&other)
                ),
            )),
        }
    }

    /// Parse YAML text.
    ///
    /// # Errors
    /// [`Error::Parse`] on malformed YAML, [`Error::InvalidValue`] if the root
    /// is not a mapping.
    pub fn from_yaml(text: &str) -> Result<Self> {
        let value: Value = serde_norway::from_str(text).map_err(|e| Error::Parse {
            kind: "mihomo config",
            path: std::path::PathBuf::from("<memory>"),
            source: Box::new(e),
        })?;
        Self::from_value(value).map_err(|e| Error::invalid("config", format!("{e}")))
    }

    /// Render to YAML.
    ///
    /// # Errors
    /// [`Error::Serialize`] if the document cannot be rendered.
    pub fn to_yaml(&self) -> Result<String> {
        serde_norway::to_string(&Value::Object(self.root.clone()))
            .map_err(|e| Error::serialize("mihomo config", e))
    }

    /// Borrow the underlying value.
    #[must_use]
    pub fn as_value(&self) -> Value {
        Value::Object(self.root.clone())
    }

    /// Borrow the underlying mapping.
    #[must_use]
    pub fn as_map(&self) -> &Map<String, Value> {
        &self.root
    }

    /// Mutably borrow the underlying mapping, for advanced edits.
    pub fn as_map_mut(&mut self) -> &mut Map<String, Value> {
        &mut self.root
    }

    /// Number of top-level keys.
    #[must_use]
    pub fn len(&self) -> usize {
        self.root.len()
    }

    /// `true` when there are no top-level keys.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.root.is_empty()
    }

    /// `true` when the key is present and not null.
    #[must_use]
    pub fn contains(&self, key: &str) -> bool {
        self.root.get(key).is_some_and(|v| !v.is_null())
    }

    /// Read a key.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.root.get(key)
    }

    /// Read a string key, accepting the types YAML commonly produces.
    #[must_use]
    pub fn get_str(&self, key: &str) -> Option<String> {
        match self.root.get(key)? {
            Value::String(s) => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            Value::Bool(b) => Some(b.to_string()),
            _ => None,
        }
    }

    /// Read an integer key.
    #[must_use]
    pub fn get_u64(&self, key: &str) -> Option<u64> {
        match self.root.get(key)? {
            Value::Number(n) => n.as_u64().or_else(|| n.as_f64().map(|f| f as u64)),
            Value::String(s) => s.parse().ok(),
            _ => None,
        }
    }

    /// Read a boolean key, tolerating the `"true"`/`1` forms seen in the wild.
    #[must_use]
    pub fn get_bool(&self, key: &str) -> Option<bool> {
        match self.root.get(key)? {
            Value::Bool(b) => Some(*b),
            Value::Number(n) => n.as_i64().map(|i| i != 0),
            Value::String(s) => match s.to_ascii_lowercase().as_str() {
                "true" | "1" | "yes" | "on" => Some(true),
                "false" | "0" | "no" | "off" => Some(false),
                _ => None,
            },
            _ => None,
        }
    }

    /// Set a key.
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<Value>) {
        self.root.insert(key.into(), value.into());
    }

    /// Remove a key, returning the previous value.
    pub fn remove(&mut self, key: &str) -> Option<Value> {
        self.root.remove(key)
    }

    /// Set a key only when it is currently absent, reporting whether it changed.
    pub fn set_default(&mut self, key: impl Into<String>, value: impl Into<Value>) -> bool {
        let key = key.into();
        if self.contains(&key) {
            return false;
        }
        self.root.insert(key, value.into());
        true
    }

    // ---------------------------------------------------------------- ports

    /// The main HTTP proxy port.
    #[must_use]
    pub fn port(&self) -> Option<u16> {
        self.get_u64("port").and_then(|v| u16::try_from(v).ok())
    }

    /// The SOCKS5 port.
    #[must_use]
    pub fn socks_port(&self) -> Option<u16> {
        self.get_u64("socks-port")
            .and_then(|v| u16::try_from(v).ok())
    }

    /// The mixed HTTP+SOCKS port.
    #[must_use]
    pub fn mixed_port(&self) -> Option<u16> {
        self.get_u64("mixed-port")
            .and_then(|v| u16::try_from(v).ok())
    }

    /// Which port a client should actually be pointed at, mirroring mihomo's
    /// own precedence: `mixed-port`, then `port`, then `socks-port`.
    #[must_use]
    pub fn effective_proxy_port(&self) -> Option<u16> {
        self.mixed_port()
            .or_else(|| self.port())
            .or_else(|| self.socks_port())
    }

    /// The controller address, e.g. `127.0.0.1:9090`.
    #[must_use]
    pub fn external_controller(&self) -> Option<String> {
        self.get_str("external-controller")
    }

    /// The controller secret, if one is configured.
    #[must_use]
    pub fn secret(&self) -> Option<String> {
        self.get_str("secret").filter(|s| !s.is_empty())
    }

    /// Working mode, defaulting to `rule` the way the core does.
    #[must_use]
    pub fn mode(&self) -> String {
        self.get_str("mode").unwrap_or_else(|| "rule".to_owned())
    }

    /// Log level, defaulting to `info`.
    #[must_use]
    pub fn log_level(&self) -> String {
        self.get_str("log-level")
            .unwrap_or_else(|| "info".to_owned())
    }

    // ------------------------------------------------------------- children

    /// Parse the `proxies` list, skipping entries that fail to parse.
    #[must_use]
    pub fn proxies(&self) -> Vec<Proxy> {
        self.parse_list("proxies")
    }

    /// Replace the `proxies` list.
    ///
    /// # Errors
    /// [`Error::Serialize`] if an entry cannot be encoded.
    pub fn set_proxies(&mut self, proxies: &[Proxy]) -> Result<()> {
        self.set_list("proxies", proxies)
    }

    /// Parse the `proxy-groups` list.
    #[must_use]
    pub fn proxy_groups(&self) -> Vec<ProxyGroup> {
        self.parse_list("proxy-groups")
    }

    /// Replace the `proxy-groups` list.
    ///
    /// # Errors
    /// [`Error::Serialize`] if an entry cannot be encoded.
    pub fn set_proxy_groups(&mut self, groups: &[ProxyGroup]) -> Result<()> {
        self.set_list("proxy-groups", groups)
    }

    /// Parse `proxy-providers`.
    #[must_use]
    pub fn proxy_providers(&self) -> Map<String, Value> {
        match self.root.get("proxy-providers") {
            Some(Value::Object(m)) => m.clone(),
            _ => Map::new(),
        }
    }

    /// Parsed `proxy-providers` entries.
    #[must_use]
    pub fn proxy_provider_list(&self) -> Vec<(String, ProxyProvider)> {
        self.proxy_providers()
            .into_iter()
            .filter_map(|(k, v)| serde_json::from_value(v).ok().map(|p| (k, p)))
            .collect()
    }

    /// Parse the `rules` list into structured rules.
    #[must_use]
    pub fn rules(&self) -> Vec<Rule> {
        let raw: Vec<String> = self
            .root
            .get("rules")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        Rule::parse_all(&raw)
    }

    /// The raw `rules` list, including comments and blanks.
    #[must_use]
    pub fn raw_rules(&self) -> Vec<String> {
        self.root
            .get("rules")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Replace the `rules` list.
    pub fn set_rules(&mut self, rules: &[Rule]) {
        let arr: Vec<Value> = rules.iter().map(|r| Value::String(r.to_string())).collect();
        self.root.insert("rules".to_owned(), Value::Array(arr));
    }

    /// Replace the `rules` list with raw strings, preserving comments.
    pub fn set_raw_rules<S: AsRef<str>>(&mut self, rules: &[S]) {
        let arr: Vec<Value> = rules
            .iter()
            .map(|r| Value::String(r.as_ref().to_owned()))
            .collect();
        self.root.insert("rules".to_owned(), Value::Array(arr));
    }

    /// `rule-providers` as a raw mapping.
    #[must_use]
    pub fn rule_providers(&self) -> Map<String, Value> {
        match self.root.get("rule-providers") {
            Some(Value::Object(m)) => m.clone(),
            _ => Map::new(),
        }
    }

    /// Names of everything `rules` references through `RULE-SET`.
    #[must_use]
    pub fn referenced_rule_sets(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .rules()
            .iter()
            .filter_map(|r| r.rule_set_name().map(str::to_owned))
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// The DNS section, if present.
    #[must_use]
    pub fn dns(&self) -> Option<&Map<String, Value>> {
        self.root.get("dns").and_then(Value::as_object)
    }

    /// The TUN section, if present.
    #[must_use]
    pub fn tun(&self) -> Option<&Map<String, Value>> {
        self.root.get("tun").and_then(Value::as_object)
    }

    /// `true` when TUN is present and enabled.
    #[must_use]
    pub fn tun_enabled(&self) -> bool {
        self.tun()
            .and_then(|t| t.get("enable"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    /// `true` when IPv6 is enabled at the top level.
    #[must_use]
    pub fn ipv6(&self) -> bool {
        self.get_bool("ipv6").unwrap_or(false)
    }

    // ---------------------------------------------------------------- stats

    /// Counts used by the UI summary pane.
    #[must_use]
    pub fn stats(&self) -> ConfigStats {
        let rules = self.rules();
        ConfigStats {
            keys: self.root.len(),
            proxies: self.proxies().len(),
            groups: self.proxy_groups().len(),
            providers: self.proxy_providers().len(),
            rules: rules.len(),
            rule_sets: self.rule_providers().len(),
            terminal_rules: rules.iter().filter(|r| r.is_terminal()).count(),
        }
    }

    // -------------------------------------------------------------- helpers

    fn parse_list<T: serde::de::DeserializeOwned>(&self, key: &str) -> Vec<T> {
        self.root
            .get(key)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| serde_json::from_value(v.clone()).ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn set_list<T: serde::Serialize>(&mut self, key: &str, items: &[T]) -> Result<()> {
        let mut arr = Vec::with_capacity(items.len());
        for item in items {
            arr.push(
                serde_json::to_value(item).map_err(|e| Error::serialize("config list entry", e))?,
            );
        }
        self.root.insert(key.to_owned(), Value::Array(arr));
        Ok(())
    }
}

/// Summary counts for a configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ConfigStats {
    /// Top-level keys.
    pub keys: usize,
    /// Declared proxies.
    pub proxies: usize,
    /// Declared proxy groups.
    pub groups: usize,
    /// Proxy providers.
    pub providers: usize,
    /// Routing rules.
    pub rules: usize,
    /// Rule providers.
    pub rule_sets: usize,
    /// Terminal (`MATCH`) rules; more than one is almost always a bug.
    pub terminal_rules: usize,
}

/// Overwrite the routing rules, replacing instead of appending.
///
/// Provided as a named function because "replace rules" and "extend rules" are
/// easy to confuse at call sites.
pub fn replace_rules(config: &mut Config, rules: &[Rule]) {
    config.set_rules(rules);
}

/// Build a minimal, valid configuration from scratch.
///
/// Useful for `cvt init` and for tests, and as the base document when a
/// subscription is a bare proxy list rather than a full config.
#[must_use]
pub fn minimal(mixed_port: u16, controller: &str) -> Config {
    let mut c = Config::empty();
    c.set("mixed-port", json!(mixed_port));
    c.set("allow-lan", json!(false));
    c.set("bind-address", json!("127.0.0.1"));
    c.set("mode", json!("rule"));
    c.set("log-level", json!("info"));
    c.set("ipv6", json!(false));
    c.set("unified-delay", json!(true));
    c.set("tcp-concurrent", json!(true));
    c.set("external-controller", json!(controller));
    c.set("proxies", json!([]));
    c.set("proxy-groups", json!([]));
    c.set("rules", json!(["MATCH,DIRECT"]));
    c
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "a list",
        Value::Object(_) => "a mapping",
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
mixed-port: 7890
allow-lan: false
mode: rule
log-level: warning
ipv6: true
external-controller: 127.0.0.1:9090
some-future-key: { a: 1, b: [2, 3] }
proxies:
  - { name: "JP 01", type: vless, server: 1.2.3.4, port: 443, uuid: u, flow: xtls-rprx-vision }
proxy-groups:
  - { name: PROXY, type: select, proxies: ["JP 01", DIRECT] }
rules:
  - DOMAIN-SUFFIX,google.com,PROXY
  - MATCH,DIRECT
"#;

    #[test]
    fn round_trips_losslessly_including_unknown_keys() {
        let c = Config::from_yaml(SAMPLE).unwrap();
        let rendered = c.to_yaml().unwrap();
        let again = Config::from_yaml(&rendered).unwrap();
        assert_eq!(c, again, "config must survive a YAML round-trip unchanged");
        assert_eq!(
            again
                .get("some-future-key")
                .and_then(|v| v.get("a"))
                .and_then(Value::as_i64),
            Some(1),
            "unknown top-level keys must be preserved"
        );
    }

    #[test]
    fn reads_typed_values_with_tolerance() {
        let c = Config::from_yaml(SAMPLE).unwrap();
        assert_eq!(c.mixed_port(), Some(7890));
        assert_eq!(c.mode(), "rule");
        assert_eq!(c.log_level(), "warning");
        assert_eq!(c.external_controller().as_deref(), Some("127.0.0.1:9090"));
        assert!(c.ipv6());
        assert!(!c.tun_enabled());
        assert_eq!(c.effective_proxy_port(), Some(7890));
    }

    #[test]
    fn parses_children() {
        let c = Config::from_yaml(SAMPLE).unwrap();
        let stats = c.stats();
        assert_eq!(stats.proxies, 1);
        assert_eq!(stats.groups, 1);
        assert_eq!(stats.rules, 2);
        assert_eq!(stats.terminal_rules, 1);
        assert_eq!(c.proxies()[0].endpoint(), "1.2.3.4:443");
        assert_eq!(c.proxy_groups()[0].proxies, vec!["JP 01", "DIRECT"]);
    }

    #[test]
    fn counts_ports_with_correct_precedence() {
        let mut c = Config::empty();
        c.set("port", json!(7891));
        c.set("socks-port", json!(7892));
        assert_eq!(c.effective_proxy_port(), Some(7891));
        c.set("mixed-port", json!(7890));
        assert_eq!(c.effective_proxy_port(), Some(7890));
    }

    #[test]
    fn rejects_non_mapping_roots() {
        assert!(Config::from_yaml("- a\n- b\n").is_err());
        assert!(Config::from_yaml("just a string").is_err());
        assert!(
            Config::from_yaml("").is_ok(),
            "an empty document is an empty config"
        );
    }

    #[test]
    fn minimal_config_is_valid_and_terminal() {
        let c = minimal(7890, "127.0.0.1:9090");
        let rules = c.rules();
        assert_eq!(rules.len(), 1);
        assert!(rules[0].is_terminal());
        assert_eq!(c.stats().terminal_rules, 1);
    }
}
