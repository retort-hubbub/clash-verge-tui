//! Pre-flight validation of a generated configuration.
//!
//! Both reference implementations hand the merged document straight to the core
//! and let it fail. mihomo reports the *first* problem it hits and then exits,
//! so a config with five faults takes five restarts to fix. Worse, a config that
//! mihomo happens to accept can still be semantically broken — unreachable rules
//! behind a `MATCH`, groups pointing at nodes that no longer exist, a `RULE-SET`
//! whose provider was renamed.
//!
//! [`check`] walks the whole document once and returns every problem it finds,
//! each with a stable code, a location, and a suggested fix. It is pure: it
//! never touches the network or the filesystem, so it runs in tests and before
//! every apply.
//!
//! # Codes
//!
//! | Prefix | Meaning |
//! |--------|---------|
//! | `E` | The core will refuse to start, or behaviour is undefined |
//! | `W` | The config works but is probably not what the author intended |
//! | `I` | Informational note |

use std::collections::{HashMap, HashSet};

use crate::model::config::Config;

/// How badly a finding affects the configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Informational only.
    Info,
    /// Accepted by the core, but likely unintended.
    Warning,
    /// The core will reject this, or the config is unusable.
    Error,
}

impl Severity {
    /// Single-character tag used in the TUI gutter.
    #[must_use]
    pub fn tag(self) -> char {
        match self {
            Self::Error => 'E',
            Self::Warning => 'W',
            Self::Info => 'i',
        }
    }

    /// Lower-case word form.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Info => "info",
        }
    }
}

/// One finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// Stable code, e.g. `E-DANGLING-POLICY`.
    pub code: &'static str,
    /// Impact.
    pub severity: Severity,
    /// What is wrong.
    pub message: String,
    /// Where it is, e.g. `rules[12]`.
    pub location: Option<String>,
    /// How to fix it.
    pub hint: Option<String>,
}

impl Diagnostic {
    fn error(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            severity: Severity::Error,
            message: message.into(),
            location: None,
            hint: None,
        }
    }

    fn warn(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            severity: Severity::Warning,
            message: message.into(),
            location: None,
            hint: None,
        }
    }

    fn info(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            severity: Severity::Info,
            message: message.into(),
            location: None,
            hint: None,
        }
    }

    fn at(mut self, loc: impl Into<String>) -> Self {
        self.location = Some(loc.into());
        self
    }

    fn fix(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

/// The result of a validation pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// Every finding, ordered by severity then discovery order.
    pub diagnostics: Vec<Diagnostic>,
}

impl Report {
    /// Number of errors.
    #[must_use]
    pub fn errors(&self) -> usize {
        self.count(Severity::Error)
    }

    /// Number of warnings.
    #[must_use]
    pub fn warnings(&self) -> usize {
        self.count(Severity::Warning)
    }

    /// Number of informational notes.
    #[must_use]
    pub fn infos(&self) -> usize {
        self.count(Severity::Info)
    }

    fn count(&self, s: Severity) -> usize {
        self.diagnostics.iter().filter(|d| d.severity == s).count()
    }

    /// `true` when nothing would stop the core from starting.
    #[must_use]
    pub fn is_ok(&self) -> bool {
        self.errors() == 0
    }

    /// Sort so errors surface first, then warnings.
    pub fn sort(&mut self) {
        self.diagnostics
            .sort_by_key(|d| std::cmp::Reverse(d.severity));
    }

    /// One-line summary, e.g. `2 errors, 3 warnings`.
    #[must_use]
    pub fn summary(&self) -> String {
        let (e, w, i) = (self.errors(), self.warnings(), self.infos());
        if e + w + i == 0 {
            return "no problems found".to_owned();
        }
        let mut parts = Vec::new();
        if e > 0 {
            parts.push(format!("{e} error{}", plural(e)));
        }
        if w > 0 {
            parts.push(format!("{w} warning{}", plural(w)));
        }
        if i > 0 {
            parts.push(format!("{i} note{}", plural(i)));
        }
        parts.join(", ")
    }

    /// Render as a block of text, one finding per line.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        for d in &self.diagnostics {
            out.push_str(d.severity.tag().to_string().as_str());
            out.push(' ');
            out.push_str(d.code);
            if let Some(loc) = &d.location {
                out.push_str(" (");
                out.push_str(loc);
                out.push(')');
            }
            out.push_str(": ");
            out.push_str(&d.message);
            if let Some(h) = &d.hint {
                out.push_str("\n    hint: ");
                out.push_str(h);
            }
            out.push('\n');
        }
        out
    }

    /// Iterator over errors only.
    pub fn errors_iter(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Policies that are always in scope, even though nothing in the document
/// defines them.
///
/// Verified against mihomo v1.19.31, which reports all seven through
/// `GET /proxies` for a document with two proxies and two groups — eleven
/// entries in total. `GLOBAL` and `PASS-RULE` are the two that are easy to
/// mistake for a typo: a rule targeting either is legal, so calling it dangling
/// would reject a configuration the core accepts and runs.
const BUILTIN_POLICIES: &[&str] = &[
    "DIRECT",
    "REJECT",
    "REJECT-DROP",
    "PASS",
    "PASS-RULE",
    "COMPATIBLE",
    "GLOBAL",
];

/// Validate a configuration, returning every problem found.
#[must_use]
pub fn check(config: &Config) -> Report {
    let mut report = Report::default();

    let proxies = config.proxies();
    let groups = config.proxy_groups();
    let rules = config.rules();
    let rule_providers = config.rule_providers();
    let proxy_providers = config.proxy_providers();

    // -- names -------------------------------------------------------------
    let mut proxy_names: HashSet<&str> = HashSet::new();
    let mut seen_proxy: HashMap<&str, usize> = HashMap::new();
    for (i, p) in proxies.iter().enumerate() {
        if seen_proxy.insert(p.name.as_str(), i).is_some() {
            report.diagnostics.push(
                Diagnostic::error(
                    "E-DUPLICATE-PROXY",
                    format!("proxy `{}` is defined more than once", p.name),
                )
                .at(format!("proxies[{i}]"))
                .fix("remove or rename the duplicate; mihomo keeps only one of them"),
            );
        }
        proxy_names.insert(p.name.as_str());
        if p.name.trim().is_empty() {
            report.diagnostics.push(
                Diagnostic::error("E-EMPTY-NAME", "a proxy has an empty name")
                    .at(format!("proxies[{i}]")),
            );
        }
        if p.server.as_deref().is_none_or(|s| s.trim().is_empty()) {
            report.diagnostics.push(
                Diagnostic::error(
                    "E-MISSING-SERVER",
                    format!("proxy `{}` has no server address", p.name),
                )
                .at(format!("proxies[{i}]")),
            );
        }
        if p.port.is_none() && p.extra.get("port").is_none() {
            report.diagnostics.push(
                Diagnostic::warn("W-MISSING-PORT", format!("proxy `{}` has no port", p.name))
                    .at(format!("proxies[{i}]")),
            );
        }
    }

    let mut group_names: HashSet<&str> = HashSet::new();
    let mut seen_group: HashMap<&str, usize> = HashMap::new();
    for (i, g) in groups.iter().enumerate() {
        if seen_group.insert(g.name.as_str(), i).is_some() {
            report.diagnostics.push(
                Diagnostic::error(
                    "E-DUPLICATE-GROUP",
                    format!("proxy group `{}` is defined more than once", g.name),
                )
                .at(format!("proxy-groups[{i}]")),
            );
        }
        group_names.insert(g.name.as_str());
    }

    // Everything a policy may legally point at.
    let provider_node_names: HashSet<&str> = proxy_providers.keys().map(String::as_str).collect();
    let is_known_policy = |name: &str| {
        BUILTIN_POLICIES
            .iter()
            .any(|b| b.eq_ignore_ascii_case(name))
            || proxy_names.contains(name)
            || group_names.contains(name)
            || provider_node_names.contains(name)
    };

    // -- groups ------------------------------------------------------------
    for (i, g) in groups.iter().enumerate() {
        if g.name.trim().is_empty() {
            report.diagnostics.push(
                Diagnostic::error("E-EMPTY-NAME", "a proxy group has an empty name")
                    .at(format!("proxy-groups[{i}]")),
            );
        }
        if g.group_kind() == crate::model::proxy::GroupKind::Unknown {
            report.diagnostics.push(
                Diagnostic::error(
                    "E-GROUP-TYPE",
                    format!("proxy group `{}` has unsupported type `{}`", g.name, g.kind),
                )
                .at(format!("proxy-groups[{i}]"))
                .fix("use one of: select, url-test, fallback, load-balance, relay, smart"),
            );
        }
        for m in &g.proxies {
            if !is_known_policy(m) {
                report.diagnostics.push(
                    Diagnostic::error(
                        "E-DANGLING-GROUP-MEMBER",
                        format!(
                            "group `{}` lists `{m}`, which is not a proxy or group",
                            g.name
                        ),
                    )
                    .at(format!("proxy-groups[{i}].proxies"))
                    .fix("a stale subscription usually causes this; update the profile"),
                );
            }
        }
        for u in &g.use_providers {
            if !proxy_providers.contains_key(u) {
                report.diagnostics.push(
                    Diagnostic::error(
                        "E-DANGLING-PROVIDER",
                        format!(
                            "group `{}` uses provider `{u}`, which is not defined",
                            g.name
                        ),
                    )
                    .at(format!("proxy-groups[{i}].use")),
                );
            }
        }
        if g.is_dynamic() && g.is_filtered() {
            // Legal, but a filter that matches nothing yields an empty group.
            let re = g.filter.as_deref().unwrap_or_default();
            if regex::Regex::new(re).is_err() {
                report.diagnostics.push(
                    Diagnostic::error(
                        "E-BAD-FILTER",
                        format!("group `{}` has an invalid filter regex `{re}`", g.name),
                    )
                    .at(format!("proxy-groups[{i}].filter")),
                );
            }
        }
        if g.proxies.is_empty() && g.use_providers.is_empty() && g.group_kind().is_testable() {
            report.diagnostics.push(
                Diagnostic::warn(
                    "W-EMPTY-GROUP",
                    format!("group `{}` has no members", g.name),
                )
                .at(format!("proxy-groups[{i}]")),
            );
        }
    }

    // -- relay cycles ------------------------------------------------------
    detect_relay_cycles(&groups, &mut report);

    // -- rules -------------------------------------------------------------
    if rules.is_empty() {
        report.diagnostics.push(
            Diagnostic::error("E-NO-RULES", "the config has no rules")
                .fix("add at least `MATCH,DIRECT` as the last rule"),
        );
    }
    let terminal: Vec<usize> = rules
        .iter()
        .enumerate()
        .filter(|(_, r)| r.is_terminal())
        .map(|(i, _)| i)
        .collect();
    if terminal.is_empty() && !rules.is_empty() {
        report.diagnostics.push(
            Diagnostic::warn(
                "W-NO-TERMINAL-RULE",
                "no `MATCH` rule; traffic that matches nothing will be rejected",
            )
            .fix("append `MATCH,DIRECT` or `MATCH,<group>` as the final rule"),
        );
    }
    // A terminal rule swallows everything after it, so anything below the first
    // `MATCH` is dead. Two distinct faults live here:
    //
    // * the `MATCH` is simply misplaced  -> warning (the core starts fine),
    // * there is a *second* `MATCH`      -> error (that rule can never fire, and
    //   it is always an editing accident, e.g. an appended profile adding its
    //   own catch-all on top of the subscription's).
    if let Some(&first) = terminal.first() {
        let dead = rules.len() - first - 1;
        if dead > 0 {
            report.diagnostics.push(
                Diagnostic::warn(
                    "W-TERMINAL-NOT-LAST",
                    format!(
                        "`{}` at rules[{first}] is not last, so the {dead} rule(s) below it can never match",
                        rules[first]
                    ),
                )
                .at(format!("rules[{first}]"))
                .fix("move the MATCH rule to the end of the list"),
            );
        }
    }
    for &idx in terminal.iter().skip(1) {
        report.diagnostics.push(
            Diagnostic::error(
                "E-UNREACHABLE-RULES",
                format!(
                    "`{}` at rules[{idx}] is a second terminal rule and can never match",
                    rules[idx]
                ),
            )
            .at(format!("rules[{idx}]"))
            .fix("delete this catch-all, or move it above the first one to make it effective"),
        );
    }

    let mut seen_rule: HashMap<String, usize> = HashMap::new();
    for (i, r) in rules.iter().enumerate() {
        if let Some(prev) = seen_rule.insert(r.to_string(), i) {
            report.diagnostics.push(
                Diagnostic::warn(
                    "W-DUPLICATE-RULE",
                    format!("rule `{r}` is identical to rules[{prev}]"),
                )
                .at(format!("rules[{i}]"))
                .fix("delete the later copy; it is never evaluated"),
            );
        }
        if !is_known_policy(&r.policy) {
            report.diagnostics.push(
                Diagnostic::error(
                    "E-DANGLING-POLICY",
                    format!("rule `{r}` points at `{}`, which does not exist", r.policy),
                )
                .at(format!("rules[{i}]"))
                .fix("check the group name for typos, or add the missing group"),
            );
        }
        if let Some(set) = r.rule_set_name()
            && !rule_providers.contains_key(set)
        {
            report.diagnostics.push(
                Diagnostic::error(
                    "E-DANGLING-RULE-SET",
                    format!("rule `{r}` uses rule-set `{set}`, which is not defined"),
                )
                .at(format!("rules[{i}]"))
                .fix("add the provider under `rule-providers` or remove the rule"),
            );
        }
        check_rule_payload(r, i, &mut report);
    }

    // -- unused declarations -----------------------------------------------
    let referenced_sets: HashSet<&str> = rules.iter().filter_map(|r| r.rule_set_name()).collect();
    for name in rule_providers.keys() {
        if !referenced_sets.contains(name.as_str()) {
            report.diagnostics.push(
                Diagnostic::info(
                    "I-UNUSED-RULE-SET",
                    format!("rule-provider `{name}` is never referenced by a rule"),
                )
                .fix("it still costs a download at start-up; remove it if unused"),
            );
        }
    }

    // -- ports -------------------------------------------------------------
    check_ports(config, &mut report);

    // -- dns / tun ---------------------------------------------------------
    check_dns(config, &mut report);
    check_tun(config, &mut report);

    report.sort();
    report
}

fn check_rule_payload(rule: &crate::model::rule::Rule, index: usize, report: &mut Report) {
    let Some(payload) = rule.payload.as_deref() else {
        return;
    };
    let loc = format!("rules[{index}]");
    match rule.kind.as_str() {
        "IP-CIDR" | "IP-CIDR6" | "SRC-IP-CIDR" => {
            if !payload.contains('/') {
                report.diagnostics.push(
                    Diagnostic::warn(
                        "W-CIDR-NO-PREFIX",
                        format!("`{rule}` has no prefix length; mihomo assumes a full-length mask"),
                    )
                    .at(loc.clone())
                    .fix("write an explicit prefix, e.g. `1.2.3.0/24`"),
                );
            }
            let want_v6 = rule.kind == "IP-CIDR6";
            let looks_v6 = payload.contains(':');
            if want_v6 != looks_v6 {
                report.diagnostics.push(
                    Diagnostic::error(
                        "E-CIDR-FAMILY",
                        format!("`{rule}` uses the wrong address family for `{}`", rule.kind),
                    )
                    .at(loc.clone())
                    .fix(if want_v6 {
                        "use `IP-CIDR` for IPv4, `IP-CIDR6` for IPv6"
                    } else {
                        "use `IP-CIDR6` for IPv6 addresses"
                    }),
                );
            }
        }
        "DOMAIN-SUFFIX" | "DOMAIN-KEYWORD" => {
            if payload.starts_with('.') || payload.starts_with("*.") {
                report.diagnostics.push(
                    Diagnostic::warn(
                        "W-DOMAIN-WILDCARD",
                        format!("`{rule}`: `DOMAIN-SUFFIX` already matches subdomains"),
                    )
                    .at(loc.clone())
                    .fix("write the bare domain, e.g. `google.com`"),
                );
            }
        }
        _ => {}
    }
    if rule.kind == "MATCH" {
        report.diagnostics.push(
            Diagnostic::error(
                "E-MATCH-WITH-PAYLOAD",
                format!("`{rule}` gives a payload to MATCH, which takes none"),
            )
            .at(loc)
            .fix("write `MATCH,<policy>`"),
        );
    }
}

fn detect_relay_cycles(groups: &[crate::model::proxy::ProxyGroup], report: &mut Report) {
    let relay: HashMap<&str, &[String]> = groups
        .iter()
        .filter(|g| g.group_kind() == crate::model::proxy::GroupKind::Relay)
        .map(|g| (g.name.as_str(), g.proxies.as_slice()))
        .collect();
    if relay.is_empty() {
        return;
    }
    // Iterative DFS with colouring: 0 unvisited, 1 on stack, 2 done.
    let mut colour: HashMap<&str, u8> = HashMap::new();
    for start in relay.keys() {
        if colour.get(start).copied().unwrap_or(0) != 0 {
            continue;
        }
        let mut stack = vec![(*start, 0usize)];
        colour.insert(start, 1);
        while let Some((node, idx)) = stack.pop() {
            let children: &[String] = relay.get(node).copied().unwrap_or(&[]);
            if idx >= children.len() {
                colour.insert(node, 2);
                continue;
            }
            stack.push((node, idx + 1));
            let child = children[idx].as_str();
            let Some(_) = relay.get_key_value(child) else {
                continue;
            };
            match colour.get(child).copied().unwrap_or(0) {
                1 => {
                    report.diagnostics.push(
                        Diagnostic::error(
                            "E-RELAY-CYCLE",
                            format!("relay group cycle detected: `{node}` -> `{child}`"),
                        )
                        .fix("a relay chain must be acyclic"),
                    );
                }
                0 => {
                    colour.insert(child, 1);
                    stack.push((child, 0));
                }
                _ => {}
            }
        }
    }
}

fn check_ports(config: &Config, report: &mut Report) {
    let mut used: HashMap<u16, &'static str> = HashMap::new();
    for (key, label) in [
        ("port", "port"),
        ("socks-port", "socks-port"),
        ("mixed-port", "mixed-port"),
        ("redir-port", "redir-port"),
        ("tproxy-port", "tproxy-port"),
    ] {
        let Some(raw) = config.get_u64(key) else {
            continue;
        };
        if raw == 0 {
            continue;
        }
        let Ok(port) = u16::try_from(raw) else {
            report.diagnostics.push(
                Diagnostic::error(
                    "E-PORT-RANGE",
                    format!("{label} is {raw}, which is not a valid TCP port"),
                )
                .fix("use a value in 1..=65535"),
            );
            continue;
        };
        if let Some(previous) = used.insert(port, label) {
            report.diagnostics.push(
                Diagnostic::error(
                    "E-PORT-CONFLICT",
                    format!("{label} and {previous} are both set to {port}"),
                )
                .fix("each listener needs its own port; mixed-port replaces port + socks-port"),
            );
        }
    }
    if config.contains("mixed-port") && (config.contains("port") || config.contains("socks-port")) {
        report.diagnostics.push(
            Diagnostic::warn(
                "W-MIXED-PORT-REDUNDANT",
                "mixed-port is set alongside port/socks-port",
            )
            .fix("prefer mixed-port alone, or the core starts two listeners"),
        );
    }
    if config.external_controller().is_none() {
        report.diagnostics.push(
            Diagnostic::warn(
                "W-NO-CONTROLLER",
                "external-controller is not set; clash-verge-tui cannot manage this core",
            )
            .fix("add `external-controller: 127.0.0.1:9090`"),
        );
    } else if let Some(addr) = config.external_controller()
        && !addr.contains(':')
    {
        report.diagnostics.push(
            Diagnostic::error(
                "E-CONTROLLER-FORMAT",
                format!("external-controller `{addr}` is missing a port"),
            )
            .fix("use `host:port`"),
        );
    }
}

fn check_dns(config: &Config, report: &mut Report) {
    let Some(dns) = config.dns() else {
        if config.tun_enabled() {
            report.diagnostics.push(
                Diagnostic::warn(
                    "W-TUN-NO-DNS",
                    "TUN is enabled but there is no `dns` section",
                )
                .fix("TUN without DNS leaks or fails closed on name resolution"),
            );
        }
        return;
    };
    let enabled = dns
        .get("enable")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if !enabled {
        report.diagnostics.push(Diagnostic::info(
            "I-DNS-DISABLED",
            "`dns.enable` is false; the core uses the system resolver",
        ));
        return;
    }
    let has_ns = dns
        .get("nameserver")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|a| !a.is_empty())
        || dns.contains_key("nameserver-policy");
    if !has_ns {
        report.diagnostics.push(
            Diagnostic::error(
                "E-DNS-NO-NAMESERVER",
                "`dns.enable` is true but no nameserver is configured",
            )
            .fix("add `nameserver: [1.1.1.1]`"),
        );
    }
    let enhanced = dns
        .get("enhanced-mode")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("redir-host");
    if enhanced == "fake-ip" && !dns.contains_key("fake-ip-range") {
        report.diagnostics.push(
            Diagnostic::warn("W-FAKEIP-NO-RANGE", "fake-ip mode without `fake-ip-range`")
                .fix("mihomo defaults to 198.18.0.1/16; set it explicitly for clarity"),
        );
    }
    if enhanced != "fake-ip" && dns.contains_key("fake-ip-range") {
        report.diagnostics.push(
            Diagnostic::warn(
                "W-FAKEIP-RANGE-IGNORED",
                format!("`fake-ip-range` is set but enhanced-mode is `{enhanced}`"),
            )
            .fix("the core ignores the range outside fake-ip mode"),
        );
    }
    // The ULA trap: a fake-ip-range6 inside fc00::/7 makes browsers treat the
    // synthesised address as local, which triggers LNA prompts and LAN rules.
    if let Some(range6) = dns
        .get("fake-ip-range6")
        .and_then(serde_json::Value::as_str)
    {
        let lower = range6.to_ascii_lowercase();
        let ula = lower.starts_with("fc")
            || lower.starts_with("fd")
            || lower.starts_with("fec")
            || lower.starts_with("fed")
            || lower.starts_with("fee")
            || lower.starts_with("fef");
        if ula {
            report.diagnostics.push(
                Diagnostic::warn(
                    "W-FAKEIP6-ULA",
                    format!("`fake-ip-range6: {range6}` is inside fc00::/7 (unique local addresses)"),
                )
                .fix("browsers treat ULA as a private network and may block it; prefer `2001:2::/64`"),
            );
        }
    }
    if config.ipv6() && !dns.contains_key("fake-ip-range6") && enhanced == "fake-ip" {
        report.diagnostics.push(
            Diagnostic::info(
                "I-FAKEIP6-IMPLICIT",
                "IPv6 is on with fake-ip but `fake-ip-range6` is unset",
            )
            .fix("some clients inject their own default, which may land in fc00::/7; set it explicitly"),
        );
    }
}

fn check_tun(config: &Config, report: &mut Report) {
    if !config.tun_enabled() {
        return;
    }
    let tun = config.tun().expect("tun_enabled implies a tun section");
    if !tun.contains_key("stack") {
        report.diagnostics.push(
            Diagnostic::info("I-TUN-NO-STACK", "TUN has no `stack`; the core uses gvisor")
                .fix("`system` is faster on Linux; `mixed` is a good default"),
        );
    }
    if tun.get("auto-route").and_then(serde_json::Value::as_bool) == Some(true)
        && config.dns().is_none()
    {
        report.diagnostics.push(
            Diagnostic::warn(
                "W-TUN-AUTOROUTE-NO-DNS",
                "TUN auto-route without DNS leaks queries",
            )
            .fix("enable `dns` with a fake-ip pool, or set `dns-hijack`"),
        );
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::model::config::Config;

    fn cfg(yaml: &str) -> Config {
        Config::from_yaml(yaml).unwrap()
    }

    fn codes(r: &Report) -> Vec<&str> {
        r.diagnostics.iter().map(|d| d.code).collect()
    }

    #[test]
    fn a_clean_config_has_no_findings() {
        let c = cfg(r#"
mixed-port: 7890
external-controller: 127.0.0.1:9090
proxies:
  - { name: "JP 01", type: vless, server: 1.2.3.4, port: 443, uuid: u }
proxy-groups:
  - { name: PROXY, type: select, proxies: ["JP 01", DIRECT] }
rules:
  - DOMAIN-SUFFIX,google.com,PROXY
  - MATCH,DIRECT
"#);
        let r = check(&c);
        assert!(r.is_ok(), "unexpected findings:\n{}", r.render());
        assert_eq!(r.errors(), 0);
        assert_eq!(
            r.warnings(),
            0,
            "clean config must be totally clean:\n{}",
            r.render()
        );
    }

    #[test]
    fn catches_dangling_policy_and_group_member() {
        let c = cfg(r#"
mixed-port: 7890
external-controller: 127.0.0.1:9090
proxy-groups:
  - { name: PROXY, type: select, proxies: [GHOST] }
rules:
  - DOMAIN-SUFFIX,google.com,NOPE
  - MATCH,DIRECT
"#);
        let r = check(&c);
        let cs = codes(&r);
        match r.diagnostics.iter().find(|d| d.code == "E-DANGLING-POLICY") {
            Some(d) => assert!(d.message.contains("NOPE"), "{}", d.message),
            None => panic!("expected a dangling policy: {:?}", codes(&r)),
        }
        assert!(cs.contains(&"E-DANGLING-GROUP-MEMBER"), "{cs:?}");
        assert!(!r.is_ok());
    }

    /// Finding F1: `GLOBAL` and `PASS-RULE` are built in, so a rule may target
    /// them without the document defining anything. The validator used to call
    /// both dangling, which rejected a configuration the core accepts — the
    /// live check installs exactly such a document and `PUT /configs` answers
    /// `204 No Content`.
    #[test]
    fn the_builtin_policies_are_not_dangling() {
        let c = cfg(r#"
mixed-port: 7890
external-controller: 127.0.0.1:9090
proxies:
  - { name: "JP 01", type: socks5, server: 1.2.3.4, port: 1080 }
proxy-groups:
  - { name: PROXY, type: select, proxies: ["JP 01", GLOBAL, PASS-RULE] }
rules:
  - DOMAIN-SUFFIX,a.example,GLOBAL
  - DOMAIN-SUFFIX,b.example,PASS-RULE
  - MATCH,PROXY
"#);
        let r = check(&c);
        assert!(
            !codes(&r).contains(&"E-DANGLING-POLICY"),
            "the core accepts these: {:?}",
            codes(&r)
        );
        assert!(
            !codes(&r).contains(&"E-DANGLING-GROUP-MEMBER"),
            "and a group may list them: {:?}",
            codes(&r)
        );

        // Every built-in, so a future edit to the list cannot drop one quietly.
        for builtin in BUILTIN_POLICIES {
            let c = cfg(&format!(
                "mixed-port: 7890\nexternal-controller: 127.0.0.1:9090\n\
                 rules:\n  - DOMAIN-SUFFIX,x.example,{builtin}\n  - MATCH,DIRECT\n"
            ));
            assert!(
                !codes(&check(&c)).contains(&"E-DANGLING-POLICY"),
                "{builtin} is built in"
            );
        }
    }

    #[test]
    fn warns_when_rules_follow_a_terminal() {
        let c = cfg(r#"
mixed-port: 7890
external-controller: 127.0.0.1:9090
rules:
  - MATCH,DIRECT
  - DOMAIN-SUFFIX,google.com,DIRECT
"#);
        let report = check(&c);
        let cs = codes(&report);
        assert!(cs.contains(&"W-TERMINAL-NOT-LAST"), "{cs:?}");
        // A misplaced (but reachable-in-principle) MATCH is not fatal; only a
        // second catch-all is.
        assert!(!cs.contains(&"E-UNREACHABLE-RULES"), "{cs:?}");
        assert!(report.is_ok(), "{}", report.render());
        let msg = &report.diagnostics[0].message;
        assert!(msg.contains("can never match"), "{msg}");
    }

    #[test]
    fn errors_on_a_second_terminal_rule() {
        let c = cfg(r#"
mixed-port: 7890
external-controller: 127.0.0.1:9090
rules:
  - MATCH,DIRECT
  - MATCH,REJECT
"#);
        let report = check(&c);
        assert!(
            codes(&report).contains(&"E-UNREACHABLE-RULES"),
            "{}",
            report.render()
        );
        assert!(!report.is_ok());
    }

    #[test]
    fn catches_dangling_rule_set() {
        let c = cfg(r#"
mixed-port: 7890
external-controller: 127.0.0.1:9090
rules:
  - RULE-SET,missing,DIRECT
  - MATCH,DIRECT
"#);
        let report = check(&c);
        assert!(codes(&report).contains(&"E-DANGLING-RULE-SET"));
    }

    #[test]
    fn flags_unused_rule_provider_as_info_only() {
        let c = cfg(r#"
mixed-port: 7890
external-controller: 127.0.0.1:9090
rule-providers:
  spare: { type: http, url: "https://x/y.yaml", path: ./spare.yaml }
rules:
  - MATCH,DIRECT
"#);
        let r = check(&c);
        assert!(codes(&r).contains(&"I-UNUSED-RULE-SET"));
        assert!(r.is_ok(), "an unused provider is not an error");
    }

    #[test]
    fn catches_port_conflicts_and_bad_ranges() {
        let c = cfg(r#"
mixed-port: 7890
port: 7890
tproxy-port: 99999
external-controller: 127.0.0.1:9090
rules: [MATCH,DIRECT]
"#);
        let report = check(&c);
        let cs = codes(&report);
        assert!(cs.contains(&"E-PORT-CONFLICT"), "{cs:?}");
        assert!(cs.contains(&"E-PORT-RANGE"), "{cs:?}");
    }

    #[test]
    fn detects_relay_cycles_and_allows_acyclic_ones() {
        let cyclic = cfg(r#"
external-controller: 127.0.0.1:9090
proxy-groups:
  - { name: A, type: relay, proxies: [B] }
  - { name: B, type: relay, proxies: [A] }
rules: [MATCH,A]
"#);
        let cyclic_report = check(&cyclic);
        assert!(
            codes(&cyclic_report).contains(&"E-RELAY-CYCLE"),
            "{}",
            cyclic_report.render()
        );

        let acyclic = cfg(r#"
external-controller: 127.0.0.1:9090
proxy-groups:
  - { name: A, type: relay, proxies: [B] }
  - { name: B, type: relay, proxies: [DIRECT] }
rules: [MATCH,A]
"#);
        let acyclic_report = check(&acyclic);
        assert!(!codes(&acyclic_report).contains(&"E-RELAY-CYCLE"));
        assert!(acyclic_report.is_ok(), "{}", acyclic_report.render());
    }

    #[test]
    fn warns_about_the_ula_fake_ip6_trap() {
        let c = cfg(r#"
mixed-port: 7890
external-controller: 127.0.0.1:9090
ipv6: true
dns:
  enable: true
  enhanced-mode: fake-ip
  fake-ip-range: 198.18.0.1/16
  fake-ip-range6: fdfe:dcba:9876::1/64
  nameserver: [1.1.1.1]
rules: [MATCH,DIRECT]
"#);
        let r = check(&c);
        assert!(codes(&r).contains(&"W-FAKEIP6-ULA"), "{}", r.render());

        let fixed = cfg(r#"
mixed-port: 7890
external-controller: 127.0.0.1:9090
ipv6: true
dns:
  enable: true
  enhanced-mode: fake-ip
  fake-ip-range: 198.18.0.1/16
  fake-ip-range6: 2001:2::/64
  nameserver: [1.1.1.1]
rules: [MATCH,DIRECT]
"#);
        let fixed_report = check(&fixed);
        assert!(
            !codes(&fixed_report).contains(&"W-FAKEIP6-ULA"),
            "{}",
            fixed_report.render()
        );
    }

    #[test]
    fn detects_cidr_family_mismatch() {
        let c = cfg(r#"
mixed-port: 7890
external-controller: 127.0.0.1:9090
rules:
  - IP-CIDR6,10.0.0.0/8,DIRECT
  - MATCH,DIRECT
"#);
        let report = check(&c);
        assert!(codes(&report).contains(&"E-CIDR-FAMILY"));
    }

    #[test]
    fn report_ranks_errors_before_warnings() {
        let c = cfg(r#"
mixed-port: 7890
port: 7890
external-controller: 127.0.0.1:9090
rules: [MATCH,NOPE]
"#);
        let mut r = check(&c);
        r.sort();
        let sevs: Vec<Severity> = r.diagnostics.iter().map(|d| d.severity).collect();
        let mut sorted = sevs.clone();
        sorted.sort_by_key(|s| std::cmp::Reverse(*s));
        assert_eq!(sevs, sorted);
        assert!(r.summary().contains("error"));
    }
}
