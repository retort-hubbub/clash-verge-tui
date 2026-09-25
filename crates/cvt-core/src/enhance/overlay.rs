//! Declarative overrides.
//!
//! `clash-verge-rev` lets a user rewrite the configuration with JavaScript.
//! That is expressive, but it brings a script engine, code that cannot be
//! reviewed in the UI, and failures that surface as a stack trace at
//! apply-time. This module is the declarative alternative: a small document
//! that says *what* to change, in a form that can be validated, diffed and
//! rendered as a list of edits.
//!
//! ```yaml
//! # overrides/office.yaml
//! remove:
//!   - "proxy-groups[name=广告拦截]"
//!
//! set:
//!   mode: rule
//!   dns.enable: true
//!   tun.enable: false
//!   "proxy-groups[name=PROXY].url": "http://cp.cloudflare.com/generate_204"
//!
//! prepend:
//!   rules:
//!     - DOMAIN-SUFFIX,intranet.example,DIRECT
//!
//! append:
//!   rules:
//!     - DOMAIN-SUFFIX,corp.example,DIRECT
//! ```
//!
//! # The two behaviours worth knowing
//!
//! **`append` keeps `rules` well-formed.** A rule appended after a `MATCH`
//! would never fire, which is the single most common way a hand-written rule
//! silently does nothing. When the target list contains a terminal rule,
//! `append` inserts immediately *before* it, so the catch-all stays last.
//! Set [`Overlay::append_before_terminal`] to `false` for the literal
//! behaviour.
//!
//! **Applying an override is idempotent — except for one case, and it is
//! worth knowing which.** `set`, `prepend`, `append` and every removal that
//! names its target (`proxy-groups[name=PROXY]`, `dns.fallback`) can be
//! applied any number of times: `prepend` and `append` never introduce a
//! duplicate, comparing structurally for mappings and by `name` for the named
//! lists, so re-running the pipeline with an unchanged subscription produces
//! an unchanged configuration. A removal by *position* (`proxies[1]`) cannot
//! make that promise, because the position is not stable: the second
//! application removes whatever has moved into the slot. It is supported
//! anyway — dropping a rule the subscription always puts first is a real
//! thing to want — but an override that uses it is one-shot, and a
//! subscription update can change what it means.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::enhance::path::{self, Path};
use crate::error::{Error, Result};

/// A declarative patch document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Overlay {
    /// Paths to delete. Deleting something absent is not an error, which keeps
    /// an override safe to re-apply.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remove: Vec<String>,
    /// `path: value` assignments.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub set: BTreeMap<String, Value>,
    /// `list-path: [items]` inserted at the front of a list.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub prepend: BTreeMap<String, Vec<Value>>,
    /// `list-path: [items]` inserted at the back of a list.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub append: BTreeMap<String, Vec<Value>>,
    /// Insert rules before a terminal rule rather than after it.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub append_before_terminal: bool,
}

fn default_true() -> bool {
    true
}

/// Written out rather than derived, and deliberately so.
///
/// `#[derive(Default)]` would set `append_before_terminal` to `false` while the
/// field's serde default is `true`, so an overlay built in code and an empty
/// overlay parsed from a document would behave differently — and the derived
/// one appends rules *after* a terminal `MATCH`, which makes them dead. Two
/// ways to spell "no overlay" have to mean the same thing.
impl Default for Overlay {
    fn default() -> Self {
        Self {
            remove: Vec::new(),
            set: BTreeMap::new(),
            prepend: BTreeMap::new(),
            append: BTreeMap::new(),
            append_before_terminal: default_true(),
        }
    }
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's skip_serializing_if signature
fn is_true(v: &bool) -> bool {
    *v
}

impl Overlay {
    /// Parse an override document.
    ///
    /// # Errors
    /// [`Error::Parse`] for malformed YAML, [`Error::InvalidValue`] when a
    /// path or a list target is malformed. Paths are checked here rather than
    /// at apply time so that a typo is reported by the editor, not by a failed
    /// apply.
    pub fn from_yaml(text: &str) -> Result<Self> {
        let overlay: Self = serde_norway::from_str(text).map_err(|e| Error::Parse {
            kind: "override profile",
            path: std::path::PathBuf::from("<memory>"),
            source: Box::new(e),
        })?;
        overlay.validate()?;
        Ok(overlay)
    }

    /// Render back to YAML.
    ///
    /// # Errors
    /// [`Error::Serialize`] on failure.
    pub fn to_yaml(&self) -> Result<String> {
        serde_norway::to_string(self).map_err(|e| Error::serialize("override profile", e))
    }

    /// Parse every path in the document.
    ///
    /// # Errors
    /// [`Error::InvalidValue`] for a malformed path or a list target that does
    /// not resolve to a key.
    pub fn validate(&self) -> Result<()> {
        for p in &self.remove {
            Path::parse(p)?;
        }
        for p in self.set.keys() {
            Path::parse(p)?;
        }
        for (p, items) in self.prepend.iter().chain(&self.append) {
            let parsed = Path::parse(p)?;
            if items.is_empty() {
                continue;
            }
            // `push` cannot target a trailing index or selector: it grows a
            // list, so the path must name the list itself.
            if matches!(
                parsed.last(),
                path::Segment::Index(_) | path::Segment::Selector { .. }
            ) {
                return Err(Error::invalid(
                    "override",
                    format!(
                        "`{p}` names a list element; a prepend/append target must name the list"
                    ),
                ));
            }
        }
        // Two catch-alls in one append list contradict each other: only one
        // can be last, and the other is a rule the user wrote that will never
        // run. Refusing says so; appending both produces a document the
        // validator rejects, and appending one produces a list that quietly
        // names fewer rules than the patch did.
        for (raw, items) in &self.append {
            let parsed = Path::parse(raw)?;
            let catch_alls = items.iter().filter(|item| is_terminal_rule(item)).count();
            if is_rules_list(&parsed) && catch_alls > 1 {
                return Err(Error::invalid(
                    "override",
                    format!(
                        "`{raw}` appends {catch_alls} catch-all rules; only the last could \
                         ever run, so write the one you mean"
                    ),
                ));
            }
        }
        self.check_targets_do_not_contradict()
    }

    /// Refuse an overlay whose entries cannot both hold.
    ///
    /// `prepend` and `append` *replace* whatever is at their path with a list —
    /// deliberately, since that is how an override grows a list the base
    /// document never had. A `set` entering or leaving that same path may need
    /// it to be a *mapping* instead, and then one of the two wins in the first
    /// pass and the other finds the wrong shape: the overlay applies once and
    /// fails the next time, which is the opposite of the idempotence this
    /// module promises.
    ///
    /// Only the genuine disagreement is refused. A `set` that reaches *into*
    /// the list (`dns.nameserver[0]`) is consistent with it being a list, and
    /// is allowed; a `set` that has to walk through it as a mapping
    /// (`dns.nameserver.foo`) is not.
    fn check_targets_do_not_contradict(&self) -> Result<()> {
        for list in self.prepend.keys().chain(self.append.keys()) {
            let list = Path::parse(list)?;
            for (raw_set, value) in &self.set {
                let set = Path::parse(raw_set)?;
                if let Some(why) = shape_conflict(&list, &set, value) {
                    return Err(Error::invalid(
                        "override",
                        format!(
                            "`{list}` is given a list while `{raw_set}` {why}; a path cannot \
                             be a list and a mapping at once"
                        ),
                    ));
                }
            }
        }
        Ok(())
    }

    /// `true` when the overlay changes nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.remove.is_empty()
            && self.set.is_empty()
            && self.prepend.is_empty()
            && self.append.is_empty()
    }

    /// Number of edits the overlay declares.
    #[must_use]
    pub fn len(&self) -> usize {
        self.remove.len()
            + self.set.len()
            + self.prepend.values().map(Vec::len).sum::<usize>()
            + self.append.values().map(Vec::len).sum::<usize>()
    }

    /// One-line description of the edits, for the profiles list.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if !self.remove.is_empty() {
            parts.push(format!("-{}", self.remove.len()));
        }
        if !self.set.is_empty() {
            parts.push(format!("~{}", self.set.len()));
        }
        let pre: usize = self.prepend.values().map(Vec::len).sum();
        if pre > 0 {
            parts.push(format!("^ {pre}"));
        }
        let app: usize = self.append.values().map(Vec::len).sum();
        if app > 0 {
            parts.push(format!("+ {app}"));
        }
        if parts.is_empty() {
            "no edits".to_owned()
        } else {
            parts.join(" ")
        }
    }

    /// Apply the overlay to a document, returning one line per edit made.
    ///
    /// Order is `remove`, then `set`, then `prepend`, then `append`: a path
    /// cleared by `remove` can be recreated by `set`, and list edits see the
    /// final scalar values.
    ///
    /// # Errors
    /// [`Error::InvalidValue`] when a `set` path cannot be resolved. List
    /// edits never fail on an absent list: they create it.
    pub fn apply(&self, config: &mut Value) -> Result<Vec<String>> {
        // The same checks a parsed document gets, so an `Overlay` built in code
        // behaves exactly like one read from a file — including refusing to
        // start work it cannot finish.
        self.validate()?;
        // Worked on a copy. An overlay is a list of operations and the later
        // ones can fail where the earlier ones succeeded, so without this a
        // rejected overlay left the part that had already landed behind — and
        // half an override is worse than none, because the half that landed
        // looks like intent. The copy costs one document; the alternative
        // costs the user a configuration nobody wrote.
        let mut candidate = config.clone();
        let log = self.apply_in_place(&mut candidate)?;
        *config = candidate;
        Ok(log)
    }

    /// The operations, in order, on a document the caller has already
    /// committed to replacing.
    fn apply_in_place(&self, config: &mut Value) -> Result<Vec<String>> {
        let mut log = Vec::new();

        for raw in &self.remove {
            let p = Path::parse(raw)?;
            if path::remove(config, &p)?.is_some() {
                log.push(format!("removed {raw}"));
            }
        }

        for (raw, value) in &self.set {
            let p = Path::parse(raw)?;
            path::set(config, &p, value.clone())?;
            log.push(format!("set {raw}"));
        }

        for (raw, items) in &self.prepend {
            let p = Path::parse(raw)?;
            log.extend(prepend_items(
                config,
                &p,
                items,
                raw,
                self.append_before_terminal && is_rules_list(&p),
            )?);
        }

        for (raw, items) in &self.append {
            let p = Path::parse(raw)?;
            log.extend(append_items(
                config,
                &p,
                items,
                raw,
                self.append_before_terminal,
            )?);
        }

        Ok(log)
    }
}

/// Insert items at the front of a list, skipping duplicates.
fn prepend_items(
    config: &mut Value,
    p: &Path,
    items: &[Value],
    raw: &str,
    tracks_terminal: bool,
) -> Result<Vec<String>> {
    let mut existing: Vec<Value> = path::get(config, p)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut fresh: Vec<Value> = Vec::new();
    let mut replaced_catch_all = false;
    for item in items {
        if is_noise(item) || contains_item(&existing, item) || contains_item(&fresh, item) {
            continue;
        }
        // A *terminal* rule is not prepended. Putting a catch-all at the top
        // of the list makes every rule below it dead — including the one the
        // base profile used as its own catch-all, which would then sit there
        // matching nothing — so it takes the existing catch-all's place
        // instead, exactly as an appended one does. The user asked for this
        // rule to be in charge; leaving the old one behind would say it is
        // while the document says otherwise.
        if tracks_terminal && is_terminal_rule(item) {
            match existing.iter().position(is_terminal_rule) {
                Some(position) => {
                    existing[position] = item.clone();
                    replaced_catch_all = true;
                }
                None => fresh.push(item.clone()),
            }
            continue;
        }
        fresh.push(item.clone());
    }
    let count = fresh.len();
    if count == 0 && !replaced_catch_all {
        return Ok(Vec::new());
    }
    fresh.extend(existing);
    replace_list(config, p, fresh)?;
    let note = if replaced_catch_all {
        format!("prepended {count} item(s) to {raw}, replacing its catch-all")
    } else {
        format!("prepended {count} item(s) to {raw}")
    };
    Ok(vec![note])
}

/// Insert items at the back of a list, before a terminal rule when asked.
fn append_items(
    config: &mut Value,
    p: &Path,
    items: &[Value],
    raw: &str,
    before_terminal: bool,
) -> Result<Vec<String>> {
    let mut existing: Vec<Value> = path::get(config, p)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    // Where an appended rule goes depends on what it is, and the position is
    // recomputed for every item: inserting one moves the catch-all, so a
    // position captured before the loop pointed at the wrong rule for the next
    // append and silently overwrote what the previous one had just added.
    //
    // A rule that is *not* terminal lands above the catch-all, or it could
    // never match anything. A rule that *is* terminal takes the catch-all's
    // place rather than stacking above it: two terminal rules in one list is a
    // document the validator rejects, and the second can never fire.
    let tracks_terminal = before_terminal && is_rules_list(p);

    let mut added = 0usize;
    for item in items {
        if is_noise(item) {
            continue;
        }
        let terminal = if tracks_terminal {
            existing.iter().position(is_terminal_rule)
        } else {
            None
        };
        let Some(position) = terminal else {
            // No catch-all to work around, so nothing constrains the order.
            if contains_item(&existing, item) {
                continue;
            }
            existing.push(item.clone());
            added += 1;
            continue;
        };
        if is_terminal_rule(item) {
            // The catch-all is replaced, never duplicated — including the case
            // where the replacement is the one that is already there.
            if existing[position] == *item {
                continue;
            }
            existing[position] = item.clone();
            added += 1;
            continue;
        }
        if contains_item(&existing, item) {
            continue;
        }
        existing.insert(position, item.clone());
        added += 1;
    }
    if added == 0 {
        return Ok(Vec::new());
    }
    replace_list(config, p, existing)?;
    Ok(vec![format!("appended {added} item(s) to {raw}")])
}

/// Whether a `set` and a list edit disagree about a path's shape.
///
/// Returns the reason when they do. Exactly one of the two can be right about
/// any given path: a list edit makes it a list, and a `set` that has to *walk
/// through* it needs a mapping. Walking through a list is possible with an
/// index or a selector (`a.b[0]`, `a[name=x]`), which is why those are not a
/// disagreement — that is how an element of a list is addressed at all.
fn shape_conflict(list: &Path, set: &Path, value: &Value) -> Option<String> {
    let list = list.segments();
    let set = set.segments();

    if list == set {
        // The two write the same path. The list edit replaces whatever is
        // there with a list, so a `set` that puts something else there is
        // describing a document that cannot exist.
        return (!value.is_array())
            .then(|| "replaces it with something that is not a list".to_owned());
    }

    if set.len() < list.len() {
        if !list.starts_with(set) {
            return None; // Unrelated paths.
        }
        // The `set` writes an ancestor, so whether the two agree depends on
        // *what* it writes: a list can be created inside a mapping and nowhere
        // else. `{a: {}}` with `append: {a.b: [...]}` is consistent and
        // common; `{a: 5}` with the same append is not.
        let mut node = value;
        for segment in &list[set.len()..] {
            let path::Segment::Key(key) = segment else {
                // Reaching into a list is fine, and the list edit is what
                // makes it a list in the first place.
                return None;
            };
            node = match node {
                // Absent is not a disagreement: it is materialised as a mapping
                // on the way down, which is what lets the list be created.
                Value::Object(map) => map.get(key)?,
                // A null is materialised as a mapping too.
                Value::Null => return None,
                _ => {
                    return Some(
                        "sets an ancestor of it to something that is not a mapping".to_owned(),
                    );
                }
            };
        }
        return (!node.is_array() && !node.is_null())
            .then(|| "sets it below to something that is not a list".to_owned());
    }

    if !set.starts_with(list) {
        return None; // Unrelated paths.
    }
    // The `set` reaches into or below the list. Only the *first* step past it
    // decides the container's shape: a key there needs a mapping, while an
    // index or a selector addresses one of the list's elements — which is
    // exactly how an element is reached, and not a disagreement at all.
    match &set[list.len()] {
        path::Segment::Key(_) => Some("needs to walk through it as a mapping".to_owned()),
        _ => None,
    }
}

/// Whether this path targets the routing rule list.
fn is_rules_list(p: &Path) -> bool {
    matches!(p.last(), path::Segment::Key(k) if k == "rules")
}

/// Whether a list element is a terminal `MATCH` rule.
fn is_terminal_rule(v: &Value) -> bool {
    v.as_str()
        .and_then(crate::model::rule::Rule::parse)
        .is_some_and(|r| r.is_terminal())
}

/// Blank strings and empty mappings are almost always a YAML typo, not intent.
fn is_noise(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::String(s) => s.trim().is_empty(),
        Value::Object(m) => m.is_empty(),
        Value::Array(a) => a.is_empty(),
        _ => false,
    }
}

/// Structural equality for mappings, `name` equality for named mappings.
fn same_item(a: &Value, b: &Value) -> bool {
    if let (Some(x), Some(y)) = (
        a.get("name").and_then(Value::as_str),
        b.get("name").and_then(Value::as_str),
    ) {
        return x == y;
    }
    a == b
}

fn contains_item(haystack: &[Value], needle: &Value) -> bool {
    haystack.iter().any(|h| same_item(h, needle))
}

/// Write a list back, creating intermediate mappings but not lists.
///
/// A failure here means the path's parent is not a mapping — for example
/// `a.b` where `a` is a string. That is a real configuration fault, so it is
/// propagated rather than swallowed.
fn replace_list(config: &mut Value, p: &Path, list: Vec<Value>) -> Result<()> {
    path::set(config, p, Value::Array(list))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    fn base() -> Value {
        json!({
            "mode": "rule",
            "log-level": "info",
            "dns": { "enable": false, "nameserver": ["1.1.1.1"], "fallback": ["8.8.8.8"] },
            "proxies": [
                {"name": "JP 01", "type": "vless", "server": "1.2.3.4", "port": 443},
                {"name": "Old Node", "type": "vless", "server": "5.6.7.8", "port": 443}
            ],
            "proxy-groups": [
                {"name": "PROXY", "type": "url-test", "url": "http://old.test", "proxies": ["JP 01"]},
                {"name": "广告拦截", "type": "select", "proxies": ["REJECT"]}
            ],
            "rules": [
                "DOMAIN-SUFFIX,google.com,PROXY",
                "MATCH,DIRECT"
            ]
        })
    }

    fn parse(yaml: &str) -> Overlay {
        Overlay::from_yaml(yaml).unwrap()
    }

    #[test]
    fn sets_scalars_and_nested_keys() {
        let o = parse(
            r#"
set:
  mode: global
  dns.enable: true
  log-level: warning
"#,
        );
        let mut c = base();
        let log = o.apply(&mut c).unwrap();
        assert_eq!(c["mode"], json!("global"));
        assert_eq!(c["dns"]["enable"], json!(true));
        assert_eq!(c["log-level"], json!("warning"));
        assert_eq!(log.len(), 3);
    }

    #[test]
    fn addresses_list_elements_by_name() {
        let o = parse(
            r#"
set:
  "proxy-groups[name=PROXY].url": "http://new.test/generate_204"
"#,
        );
        let mut c = base();
        o.apply(&mut c).unwrap();
        assert_eq!(
            c["proxy-groups"][0]["url"],
            json!("http://new.test/generate_204")
        );
        assert_eq!(
            c["proxy-groups"][1]["url"],
            Value::Null,
            "the other group is untouched"
        );
    }

    #[test]
    fn removes_a_named_group_and_a_nested_key() {
        let o = parse(
            r#"
remove:
  - "proxy-groups[name=广告拦截]"
  - dns.fallback
  - proxies[1]
"#,
        );
        let mut c = base();
        o.apply(&mut c).unwrap();
        assert_eq!(c["proxy-groups"].as_array().unwrap().len(), 1);
        assert!(!c["dns"].as_object().unwrap().contains_key("fallback"));
        assert_eq!(c["proxies"].as_array().unwrap().len(), 1);
        assert_eq!(c["proxies"][0]["name"], json!("JP 01"));
    }

    #[test]
    fn removing_something_absent_is_silent() {
        let o = parse("remove:\n  - nothing.here\n  - absent\n");
        let mut c = base();
        assert!(o.apply(&mut c).unwrap().is_empty());
        assert_eq!(c, base(), "the document is unchanged");
    }

    #[test]
    fn append_keeps_the_catch_all_rule_last() {
        // The headline behaviour: appending naively would put this rule after
        // MATCH, where it can never fire.
        let o = parse(
            r#"
append:
  rules:
    - DOMAIN-SUFFIX,corp.example,DIRECT
"#,
        );
        let mut c = base();
        o.apply(&mut c).unwrap();
        let rules: Vec<&str> = c["rules"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(
            rules,
            vec![
                "DOMAIN-SUFFIX,google.com,PROXY",
                "DOMAIN-SUFFIX,corp.example,DIRECT",
                "MATCH,DIRECT"
            ],
            "the new rule must land before MATCH"
        );
    }

    #[test]
    fn append_can_be_made_literal() {
        let o = parse(
            "append_before_terminal: false\nappend:\n  rules:\n    - DOMAIN-SUFFIX,x.test,DIRECT\n",
        );
        let mut c = base();
        o.apply(&mut c).unwrap();
        let rules: Vec<&str> = c["rules"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(rules.last(), Some(&"DOMAIN-SUFFIX,x.test,DIRECT"));
    }

    #[test]
    fn prepend_puts_rules_first() {
        let o = parse("prepend:\n  rules:\n    - DOMAIN-SUFFIX,intranet.example,DIRECT\n");
        let mut c = base();
        o.apply(&mut c).unwrap();
        assert_eq!(
            c["rules"][0],
            json!("DOMAIN-SUFFIX,intranet.example,DIRECT"),
            "prepended rules take precedence"
        );
        assert_eq!(c["rules"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn applying_twice_changes_nothing_the_second_time() {
        let o = parse(
            r#"
prepend:
  rules: ["DOMAIN-SUFFIX,intranet.example,DIRECT"]
append:
  rules: ["DOMAIN-SUFFIX,corp.example,DIRECT"]
  proxies: [{"name": "Extra", "type": "socks5", "server": "1.2.3.4", "port": 1080}]
"#,
        );
        let mut once = base();
        o.apply(&mut once).unwrap();
        let mut twice = once.clone();
        let log = o.apply(&mut twice).unwrap();
        assert_eq!(once, twice, "an override must be idempotent");
        assert!(
            log.is_empty(),
            "and must report that it did nothing: {log:?}"
        );
    }

    #[test]
    fn a_duplicate_is_not_added() {
        let o = parse("append:\n  rules:\n    - DOMAIN-SUFFIX,google.com,PROXY\n");
        let mut c = base();
        assert!(o.apply(&mut c).unwrap().is_empty());
        assert_eq!(c["rules"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn a_proxy_is_deduplicated_by_name_not_by_value() {
        // The same node with a different port must still count as present:
        // subscriptions rename servers, and a duplicate name breaks mihomo.
        let o = parse(
            r#"
append:
  proxies:
    - {name: "JP 01", type: vless, server: "9.9.9.9", port: 8443}
"#,
        );
        let mut c = base();
        assert!(o.apply(&mut c).unwrap().is_empty());
        assert_eq!(c["proxies"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn blank_items_are_ignored() {
        let o = parse(
            "append:\n  rules:\n    - \"\"\n    - null\n    - DOMAIN-SUFFIX,real.test,DIRECT\n",
        );
        let mut c = base();
        o.apply(&mut c).unwrap();
        let rules = c["rules"].as_array().unwrap();
        assert_eq!(rules.len(), 3);
        assert!(rules.iter().all(|r| !r.as_str().unwrap_or("x").is_empty()));
    }

    #[test]
    fn creates_a_list_that_does_not_exist_yet() {
        let o = parse("append:\n  rule-providers:\n    - {name: extra}\n");
        let mut c = base();
        o.apply(&mut c).unwrap();
        assert_eq!(c["rule-providers"][0]["name"], json!("extra"));
    }

    #[test]
    fn paths_are_validated_at_parse_time_not_apply_time() {
        let err = Overlay::from_yaml("set:\n  \"a[\": 1\n").unwrap_err();
        assert!(err.to_string().contains("unclosed"), "{err}");

        let err = Overlay::from_yaml("append:\n  \"rules[0]\": [x]\n").unwrap_err();
        assert!(err.to_string().contains("must name the list"), "{err}");
    }

    #[test]
    fn unknown_keys_are_rejected_rather_than_ignored() {
        // A typo such as `appends:` would otherwise silently do nothing.
        let err = Overlay::from_yaml("appends:\n  rules: [x]\n").unwrap_err();
        assert!(
            err.to_string().contains("unknown field") || err.to_string().contains("appends"),
            "{err}"
        );
    }

    #[test]
    fn a_failing_set_leaves_the_document_untouched() {
        let o = parse("set:\n  \"proxies[99].name\": nope\n");
        let mut c = base();
        assert!(o.apply(&mut c).is_err());
        assert_eq!(c, base(), "a rejected override must be a complete no-op");
    }

    #[test]
    fn summaries_describe_the_edit_kinds() {
        let o = parse(
            r#"
remove: ["dns.fallback"]
set: {mode: global}
prepend: {rules: ["A,DIRECT"]}
append: {rules: ["B,DIRECT", "C,DIRECT"]}
"#,
        );
        let s = o.summary();
        assert!(s.contains("-1"), "{s}");
        assert!(s.contains("~1"), "{s}");
        assert!(s.contains("^ 1"), "{s}");
        assert!(s.contains("+ 2"), "{s}");
        assert_eq!(o.len(), 5);
        assert!(!o.is_empty());
        assert!(Overlay::default().is_empty());
        assert_eq!(Overlay::default().summary(), "no edits");
    }

    #[test]
    fn round_trips_through_yaml() {
        let o = parse("set: {mode: global}\nremove: [x.y]\n");
        let yaml = o.to_yaml().unwrap();
        assert_eq!(Overlay::from_yaml(&yaml).unwrap(), o);
    }

    #[test]
    fn list_edits_work_on_a_nested_path() {
        let o = parse("append:\n  \"dns.nameserver\": [\"9.9.9.9\"]\n");
        let mut c = base();
        o.apply(&mut c).unwrap();
        assert_eq!(c["dns"]["nameserver"], json!(["1.1.1.1", "9.9.9.9"]));
    }

    /// A path cannot be a list and a mapping at once, and an overlay that asks
    /// for both used to apply once and then fail on the second pass — the
    /// opposite of the idempotence this module promises.
    #[test]
    fn an_overlay_that_contradicts_itself_is_refused_before_it_starts() {
        let mut c = json!({});
        let o = Overlay::from_yaml(
            "set:\n  \"dns.nameserver\": \"1.1.1.1\"\nappend:\n  dns: [\"8.8.8.8\"]\n",
        );
        // Caught when the document is read, so the editor reports it.
        match o {
            Err(e) => assert!(e.to_string().contains("list"), "{e}"),
            Ok(o) => panic!("expected the contradiction to be refused: {o:?}"),
        }

        // And caught again for an overlay built in code, which never went
        // through `from_yaml`.
        let built = Overlay {
            set: BTreeMap::from([("dns.nameserver".to_owned(), json!("1.1.1.1"))]),
            append: BTreeMap::from([("dns".to_owned(), vec![json!("8.8.8.8")])]),
            ..Overlay::default()
        };
        let err = built.apply(&mut c).unwrap_err();
        assert!(err.to_string().contains("list"), "{err}");
        assert_eq!(
            c,
            json!({}),
            "a refused overlay must not touch the document"
        );

        // The shape that is fine: two different paths.
        let fine = Overlay {
            set: BTreeMap::from([("dns.nameserver".to_owned(), json!("1.1.1.1"))]),
            append: BTreeMap::from([("rules".to_owned(), vec![json!("MATCH,DIRECT")])]),
            ..Overlay::default()
        };
        fine.apply(&mut c).unwrap();
    }

    /// Finding F17: the derived `Default` disagreed with the serde default for
    /// `append_before_terminal`, so `Overlay::default()` appended rules *after*
    /// a terminal `MATCH` — dead code — while an empty document did not.
    #[test]
    fn a_default_overlay_is_the_same_as_an_empty_document() {
        assert_eq!(
            Overlay::default(),
            Overlay::from_yaml("{}").unwrap(),
            "two ways to spell `no overlay` must mean the same thing"
        );
        assert!(
            Overlay::default().append_before_terminal,
            "appending after a terminal MATCH would make the rule dead"
        );
    }
}
