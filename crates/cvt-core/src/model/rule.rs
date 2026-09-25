//! Rule parsing and rendering.
//!
//! mihomo rules are comma separated strings:
//!
//! ```text
//! TYPE,PAYLOAD,POLICY[,PARAM...]
//! DOMAIN-SUFFIX,google.com,PROXY
//! IP-CIDR,10.0.0.0/8,DIRECT,no-resolve
//! MATCH,PROXY
//! AND,((DOMAIN,a.example),(NETWORK,udp)),PROXY
//! ```
//!
//! Two details make naive `split(',')` wrong:
//!
//! * logical rules (`AND`, `OR`, `NOT`) carry nested parenthesised rules whose
//!   payloads contain commas, and
//! * `MATCH`/`FINAL` have no payload at all, so the policy sits in field 1.
//!
//! [`Rule::parse`] handles both.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A single routing rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    /// Rule type, upper-cased (`DOMAIN-SUFFIX`, `GEOIP`, `MATCH`, ...).
    pub kind: String,
    /// The value the rule matches, if the type takes one.
    pub payload: Option<String>,
    /// Target policy: a proxy name, a group name, `DIRECT`, or `REJECT`.
    pub policy: String,
    /// Trailing modifiers such as `no-resolve`, `src`, `dst`.
    pub params: Vec<String>,
}

/// Rule types that take no payload, where the second field is the policy.
///
/// `MATCH` alone. `FINAL` is not a mihomo rule kind — `mihomo -t` answers
/// `[FINAL,DIRECT] error: format invalid` — and a validator that accepts it
/// passes a configuration the core refuses to load.
const PAYLOADLESS: &[&str] = &["MATCH"];

impl Rule {
    /// Build a rule from its parts.
    #[must_use]
    pub fn new(
        kind: impl Into<String>,
        payload: Option<impl Into<String>>,
        policy: impl Into<String>,
    ) -> Self {
        Self {
            kind: kind.into().to_ascii_uppercase(),
            payload: payload.map(Into::into),
            policy: policy.into(),
            params: Vec::new(),
        }
    }

    /// Parse one rule string.
    ///
    /// Returns `None` for blank input or comments (`#`), which callers should
    /// skip rather than treat as errors.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            return None;
        }
        let parts = split_top_level(line);
        if parts.is_empty() {
            return None;
        }
        let kind = parts[0].trim().to_ascii_uppercase();
        if PAYLOADLESS.contains(&kind.as_str()) {
            // `MATCH,POLICY`. A bare `MATCH` is refused rather than filled in
            // with `DIRECT`: the core answers it with `format invalid`, and
            // inventing a policy made the round trip produce a rule the user
            // did not write.
            let policy = parts.get(1).map(|s| s.trim()).filter(|s| !s.is_empty())?;
            return Some(Self {
                kind,
                payload: None,
                policy: policy.to_owned(),
                // What follows the policy is kept rather than dropped. The
                // core ignores it for `MATCH`, but a parser that accepts an
                // input and hands back a different one is the single thing a
                // lossless round trip cannot survive — and dropping the field
                // here is also what used to make the advisory about it
                // unreachable.
                params: parts
                    .get(2..)
                    .unwrap_or_default()
                    .iter()
                    .map(|s| s.trim().to_owned())
                    .collect(),
            });
        }
        if parts.len() < 3 {
            return None;
        }
        Some(Self {
            kind,
            payload: Some(parts[1].trim().to_owned()),
            policy: parts[2].trim().to_owned(),
            params: parts[3..].iter().map(|s| s.trim().to_owned()).collect(),
        })
    }

    /// Parse a whole block of rules, skipping blanks and comments.
    #[must_use]
    pub fn parse_all<S: AsRef<str>>(raw: &[S]) -> Vec<Self> {
        raw.iter().filter_map(|l| Self::parse(l.as_ref())).collect()
    }

    /// `true` when the rule has no payload and always matches.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        PAYLOADLESS.contains(&self.kind.as_str())
    }

    /// `true` when the rule delegates to a named sub-rule set.
    #[must_use]
    pub fn is_rule_set(&self) -> bool {
        self.kind == "RULE-SET"
    }

    /// `true` when `no-resolve` is present.
    #[must_use]
    pub fn no_resolve(&self) -> bool {
        self.params
            .iter()
            .any(|p| p.eq_ignore_ascii_case("no-resolve"))
    }

    /// Name of the rule-set provider this rule references, if any.
    #[must_use]
    pub fn rule_set_name(&self) -> Option<&str> {
        self.payload.as_deref().filter(|_| self.is_rule_set())
    }

    /// `true` when the policy is one of the two built-in targets.
    #[must_use]
    pub fn is_builtin_policy(&self) -> bool {
        self.policy.eq_ignore_ascii_case("DIRECT")
            || self.policy.eq_ignore_ascii_case("REJECT")
            || self.policy.eq_ignore_ascii_case("REJECT-DROP")
            || self.policy.eq_ignore_ascii_case("PASS")
    }
}

impl fmt::Display for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.kind)?;
        if let Some(p) = &self.payload {
            write!(f, ",{p}")?;
        }
        write!(f, ",{}", self.policy)?;
        for p in &self.params {
            write!(f, ",{p}")?;
        }
        Ok(())
    }
}

/// Split on commas that are not nested inside parentheses.
///
/// `((DOMAIN,a.example),(NETWORK,udp)),PROXY` yields three fields, not five.
#[must_use]
pub fn split_top_level(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for ch in s.chars() {
        match ch {
            '"' | '\'' if quote == Some(ch) => {
                quote = None;
                cur.push(ch);
            }
            '"' | '\'' if quote.is_none() => {
                quote = Some(ch);
                cur.push(ch);
            }
            '(' if quote.is_none() => {
                depth += 1;
                cur.push(ch);
            }
            ')' if quote.is_none() => {
                depth = depth.saturating_sub(1);
                cur.push(ch);
            }
            ',' if depth == 0 && quote.is_none() => {
                out.push(std::mem::take(&mut cur));
            }
            _ => cur.push(ch),
        }
    }
    if !cur.is_empty() || !out.is_empty() {
        out.push(cur);
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_rule() {
        let r = Rule::parse("DOMAIN-SUFFIX,google.com,PROXY").unwrap();
        assert_eq!(r.kind, "DOMAIN-SUFFIX");
        assert_eq!(r.payload.as_deref(), Some("google.com"));
        assert_eq!(r.policy, "PROXY");
        assert!(r.params.is_empty());
    }

    #[test]
    fn parses_rule_with_params() {
        let r = Rule::parse("IP-CIDR,10.0.0.0/8,DIRECT,no-resolve").unwrap();
        assert_eq!(r.params, vec!["no-resolve"]);
        assert!(r.no_resolve());
    }

    #[test]
    fn parses_match_without_payload() {
        let r = Rule::parse("MATCH,漏网之鱼").unwrap();
        assert_eq!(r.kind, "MATCH");
        assert!(r.payload.is_none());
        assert_eq!(r.policy, "漏网之鱼");
        assert!(r.is_terminal());
    }

    #[test]
    fn parses_logical_rule_with_nested_commas() {
        let src = "AND,((DOMAIN,a.example),(NETWORK,udp)),PROXY";
        let r = Rule::parse(src).unwrap();
        assert_eq!(r.kind, "AND");
        assert_eq!(
            r.payload.as_deref(),
            Some("((DOMAIN,a.example),(NETWORK,udp))")
        );
        assert_eq!(r.policy, "PROXY");
        assert_eq!(r.to_string(), src, "round-trip must be byte-exact");
    }

    #[test]
    fn round_trips_every_shape() {
        for src in [
            "DOMAIN-SUFFIX,google.com,PROXY",
            "MATCH,DIRECT",
            "IP-CIDR6,2001:db8::/32,DIRECT,no-resolve",
            "RULE-SET,reject,REJECT-DROP",
            "AND,((DOMAIN-SUFFIX,a.com),(NETWORK,tcp)),PROXY",
            "NOT,((GEOIP,CN)),PROXY",
            "SRC-PORT,80,PROXY,src",
        ] {
            let r = Rule::parse(src).unwrap_or_else(|| panic!("failed to parse {src}"));
            assert_eq!(r.to_string(), src, "round-trip failed for {src}");
        }
    }

    #[test]
    fn skips_blank_and_comment_lines() {
        assert!(Rule::parse("").is_none());
        assert!(Rule::parse("   ").is_none());
        assert!(Rule::parse("# a comment").is_none());
    }

    #[test]
    fn ignores_commas_inside_quotes() {
        let parts = split_top_level("DOMAIN,\"a,b.com\",PROXY");
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[1], "\"a,b.com\"");
    }
}
