//! Structural diffing of two configuration documents.
//!
//! Nothing in the reference implementations lets you see what a merge or an
//! override actually did before it reaches the core. A profile update can
//! silently replace a hand-written rule, and the only way to notice is that
//! traffic starts going somewhere unexpected.
//!
//! [`diff`] produces a reviewable change list. It understands the two shapes
//! that matter in a mihomo config and diffs them by identity rather than by
//! position:
//!
//! * lists of scalars (`rules`) diff as sets, so re-ordering is reported as a
//!   re-order instead of as a hundred removals plus a hundred additions;
//! * lists of mappings with a `name` (`proxies`, `proxy-groups`) diff by name,
//!   so "one node was replaced" stays one line.

use std::fmt;

use serde_json::{Map, Value};

/// What happened at one path.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    /// The node exists only in the new document.
    Added(Value),
    /// The node exists only in the old document.
    Removed(Value),
    /// The node exists in both with different values.
    Changed {
        /// Previous value.
        from: Value,
        /// New value.
        to: Value,
    },
    /// A list's contents are identical but its order changed.
    Reordered,
}

impl Change {
    /// Short verb used in summaries and list views.
    #[must_use]
    pub fn verb(&self) -> &'static str {
        match self {
            Self::Added(_) => "+",
            Self::Removed(_) => "-",
            Self::Changed { .. } => "~",
            Self::Reordered => "=",
        }
    }
}

/// One entry in a diff.
#[derive(Debug, Clone, PartialEq)]
pub struct DiffEntry {
    /// Dotted path with list suffixes, e.g. `rules[+3]` or
    /// `proxy-groups[name=PROXY].url`.
    pub path: String,
    /// What changed.
    pub change: Change,
}

/// A complete diff, possibly truncated.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Diff {
    /// The changes, in document order.
    pub entries: Vec<DiffEntry>,
    /// `true` when the entry limit was reached and more differences exist.
    pub truncated: bool,
}

impl Diff {
    /// `true` when the documents are equivalent.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Counts of each kind of change, ignoring [`Change::Reordered`].
    #[must_use]
    pub fn counts(&self) -> (usize, usize, usize) {
        let mut added = 0;
        let mut removed = 0;
        let mut changed = 0;
        for e in &self.entries {
            match e.change {
                Change::Added(_) => added += 1,
                Change::Removed(_) => removed += 1,
                Change::Changed { .. } => changed += 1,
                Change::Reordered => {}
            }
        }
        (added, removed, changed)
    }

    /// One-line summary, e.g. `+3 rules, ~1 dns`.
    #[must_use]
    pub fn summary(&self) -> String {
        if self.is_empty() {
            return "no changes".to_owned();
        }
        let (a, r, c) = self.counts();
        let mut parts = Vec::new();
        if a > 0 {
            parts.push(format!("+{a}"));
        }
        if r > 0 {
            parts.push(format!("-{r}"));
        }
        if c > 0 {
            parts.push(format!("~{c}"));
        }
        if self.truncated {
            parts.push("(truncated)".to_owned());
        }
        parts.join(" ")
    }

    /// Entries touching a given top-level key.
    #[must_use]
    pub fn for_top_level(&self, key: &str) -> Vec<&DiffEntry> {
        self.entries
            .iter()
            .filter(|e| e.path == key || e.path.starts_with(&format!("{key}.")))
            .collect()
    }

    /// Top-level keys that changed, in document order.
    #[must_use]
    pub fn touched_keys(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for e in &self.entries {
            let key = e
                .path
                .split(['.', '['])
                .next()
                .unwrap_or(&e.path)
                .to_owned();
            if !out.contains(&key) {
                out.push(key);
            }
        }
        out
    }
}

impl fmt::Display for Diff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_empty() {
            return writeln!(f, "no changes");
        }
        for e in &self.entries {
            match &e.change {
                Change::Added(v) => writeln!(f, "+ {} = {}", e.path, compact(v))?,
                Change::Removed(v) => writeln!(f, "- {} was {}", e.path, compact(v))?,
                Change::Changed { from, to } => {
                    writeln!(f, "~ {}: {} -> {}", e.path, compact(from), compact(to))?;
                }
                Change::Reordered => writeln!(f, "= {} reordered", e.path)?,
            }
        }
        if self.truncated {
            writeln!(f, "... (more changes not shown)")?;
        }
        Ok(())
    }
}

/// Default cap on the number of entries produced.
pub const DEFAULT_LIMIT: usize = 500;

/// Diff two documents, stopping after [`DEFAULT_LIMIT`] entries.
#[must_use]
pub fn diff(before: &Value, after: &Value) -> Diff {
    diff_limited(before, after, DEFAULT_LIMIT)
}

/// Diff two documents with an explicit entry limit.
#[must_use]
pub fn diff_limited(before: &Value, after: &Value, limit: usize) -> Diff {
    let mut out = Diff::default();
    walk(before, after, String::new(), &mut out, limit);
    if out.entries.len() >= limit {
        out.truncated = true;
        out.entries.truncate(limit);
    }
    out
}

fn full(out: &Diff) -> bool {
    out.truncated
}

fn walk(before: &Value, after: &Value, path: String, out: &mut Diff, limit: usize) {
    if full(out) || before == after {
        return;
    }
    match (before, after) {
        (Value::Object(b), Value::Object(a)) => {
            let mut keys: Vec<&String> = b.keys().collect();
            for k in a.keys() {
                if !b.contains_key(k) {
                    keys.push(k);
                }
            }
            for key in keys {
                if full(out) {
                    return;
                }
                let child = join_key(&path, key);
                match (b.get(key), a.get(key)) {
                    (None, Some(v)) => push(out, child, Change::Added(v.clone()), limit),
                    (Some(v), None) => push(out, child, Change::Removed(v.clone()), limit),
                    (Some(x), Some(y)) => walk(x, y, child, out, limit),
                    (None, None) => {}
                }
            }
        }
        (Value::Array(b), Value::Array(a)) => walk_arrays(b, a, path, out, limit),
        _ => push(
            out,
            path,
            Change::Changed {
                from: before.clone(),
                to: after.clone(),
            },
            limit,
        ),
    }
}

fn walk_arrays(before: &[Value], after: &[Value], path: String, out: &mut Diff, limit: usize) {
    // Shape 1: named mappings (proxies, proxy-groups, rule-providers entries).
    if let (Some(b), Some(a)) = (named(before), named(after)) {
        for (name, item) in &a {
            if full(out) {
                return;
            }
            let child = join_selector(&path, name);
            match b.get(name) {
                None => push(out, child, Change::Added(item.clone()), limit),
                Some(old) => walk(old, item, child, out, limit),
            }
        }
        for (name, item) in &b {
            if full(out) {
                return;
            }
            if !a.contains_key(name) {
                push(
                    out,
                    join_selector(&path, name),
                    Change::Removed(item.clone()),
                    limit,
                );
            }
        }
        return;
    }

    // Shape 2: lists of scalars (rules, nameserver, ...). Diff as multisets so
    // that a reorder is not reported as a full replacement.
    if before.iter().all(is_scalar) && after.iter().all(is_scalar) {
        let before_set: Vec<&Value> = before.iter().collect();
        let added: Vec<&Value> = after.iter().filter(|v| !before_set.contains(v)).collect();
        let removed: Vec<&Value> = before.iter().filter(|v| !after.contains(v)).collect();
        if added.is_empty() && removed.is_empty() {
            push(out, path, Change::Reordered, limit);
            return;
        }
        for v in added {
            if full(out) {
                return;
            }
            push(out, format!("{path}[+]"), Change::Added(v.clone()), limit);
        }
        for v in removed {
            if full(out) {
                return;
            }
            push(out, format!("{path}[-]"), Change::Removed(v.clone()), limit);
        }
        return;
    }

    // Anything else is reported as one wholesale change.
    push(
        out,
        path,
        Change::Changed {
            from: Value::Array(before.to_vec()),
            to: Value::Array(after.to_vec()),
        },
        limit,
    );
}

/// Index a list of mappings by their `name`, when every element has one.
fn named(items: &[Value]) -> Option<Map<String, Value>> {
    if items.is_empty() {
        return None;
    }
    let mut out = Map::new();
    for item in items {
        let name = item.get("name")?.as_str()?.to_owned();
        out.insert(name, item.clone());
    }
    Some(out)
}

fn is_scalar(v: &Value) -> bool {
    !v.is_array() && !v.is_object()
}

fn push(out: &mut Diff, path: String, change: Change, limit: usize) {
    if out.entries.len() >= limit {
        out.truncated = true;
        return;
    }
    out.entries.push(DiffEntry { path, change });
}

fn join_key(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_owned()
    } else {
        format!("{path}.{key}")
    }
}

fn join_selector(path: &str, name: &str) -> String {
    format!("{path}[name={name}]")
}

fn compact(v: &Value) -> String {
    let s = v.to_string();
    if s.chars().count() > 80 {
        let mut out: String = s.chars().take(77).collect();
        out.push_str("...");
        out
    } else {
        s
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn identical_documents_have_no_diff() {
        let v = json!({"a": 1, "b": [1, 2]});
        assert!(diff(&v, &v).is_empty());
        assert_eq!(diff(&v, &v).summary(), "no changes");
    }

    #[test]
    fn reports_added_and_removed_top_level_keys() {
        let d = diff(&json!({"a": 1, "gone": 2}), &json!({"a": 1, "new": 3}));
        assert_eq!(d.len(), 2);
        assert!(
            d.entries
                .iter()
                .any(|e| e.path == "new" && matches!(e.change, Change::Added(_)))
        );
        assert!(
            d.entries
                .iter()
                .any(|e| e.path == "gone" && matches!(e.change, Change::Removed(_)))
        );
    }

    #[test]
    fn recurses_into_nested_mappings() {
        let d = diff(
            &json!({"dns": {"enable": false, "nameserver": ["1.1.1.1"]}}),
            &json!({"dns": {"enable": true, "nameserver": ["1.1.1.1"]}}),
        );
        assert_eq!(d.len(), 1);
        assert_eq!(d.entries[0].path, "dns.enable");
        assert!(matches!(d.entries[0].change, Change::Changed { .. }));
    }

    #[test]
    fn diffs_rule_lists_as_multisets_not_positions() {
        let before = json!({"rules": ["A,DIRECT", "B,DIRECT", "MATCH,PROXY"]});
        let after = json!({"rules": ["B,DIRECT", "A,DIRECT", "MATCH,PROXY", "C,DIRECT"]});
        let d = diff(&before, &after);
        let added: Vec<&DiffEntry> = d
            .entries
            .iter()
            .filter(|e| matches!(e.change, Change::Added(_)))
            .collect();
        let removed = d
            .entries
            .iter()
            .filter(|e| matches!(e.change, Change::Removed(_)))
            .count();
        assert_eq!(added.len(), 1, "{:#?}", d.entries);
        assert_eq!(removed, 0, "a pure reorder must not look like a removal");
        assert_eq!(added[0].path, "rules[+]");
    }

    #[test]
    fn reports_reorder_separately_from_content_change() {
        let d = diff(&json!({"r": ["a", "b"]}), &json!({"r": ["b", "a"]}));
        assert_eq!(d.len(), 1);
        assert_eq!(d.entries[0].change, Change::Reordered);
        assert_eq!(
            d.counts(),
            (0, 0, 0),
            "a reorder is none of added/removed/changed"
        );
    }

    #[test]
    fn diffs_named_lists_by_name_not_index() {
        let before = json!({"proxy-groups": [
            {"name": "PROXY", "type": "select", "url": "http://old"},
            {"name": "GONE", "type": "select"}
        ]});
        let after = json!({"proxy-groups": [
            {"name": "NEW", "type": "select"},
            {"name": "PROXY", "type": "select", "url": "http://new"}
        ]});
        let d = diff(&before, &after);
        assert_eq!(d.len(), 3, "{:#?}", d.entries);
        assert!(
            d.entries
                .iter()
                .any(|e| e.path == "proxy-groups[name=PROXY].url"
                    && matches!(e.change, Change::Changed { .. }))
        );
        assert!(
            d.entries
                .iter()
                .any(|e| e.path == "proxy-groups[name=NEW]" && matches!(e.change, Change::Added(_)))
        );
        assert!(
            d.entries
                .iter()
                .any(|e| e.path == "proxy-groups[name=GONE]"
                    && matches!(e.change, Change::Removed(_)))
        );
    }

    #[test]
    fn a_replaced_node_is_one_line_not_a_whole_list_change() {
        let before = json!({"proxies": [{"name": "A", "port": 1}, {"name": "B", "port": 2}]});
        let after = json!({"proxies": [{"name": "A", "port": 443}, {"name": "B", "port": 2}]});
        let d = diff(&before, &after);
        assert_eq!(d.len(), 1);
        assert_eq!(d.entries[0].path, "proxies[name=A].port");
        assert_eq!(d.summary(), "~1");
    }

    #[test]
    fn truncates_and_says_so() {
        let before = json!({"rules": []});
        let after = json!({"rules": (0..100).map(|i| format!("R{i},DIRECT")).collect::<Vec<_>>()});
        let d = diff_limited(&before, &after, 10);
        assert_eq!(d.len(), 10);
        assert!(d.truncated);
        assert!(d.summary().contains("truncated"));
    }

    #[test]
    fn touch_keys_are_reported_in_document_order() {
        let d = diff(
            &json!({"mode": "rule", "dns": {"enable": false}}),
            &json!({"mode": "global", "dns": {"enable": true}}),
        );
        let keys = d.touched_keys();
        assert_eq!(keys, vec!["mode", "dns"]);
    }

    #[test]
    fn display_is_human_readable() {
        let d = diff(&json!({"mode": "rule"}), &json!({"mode": "global"}));
        let s = d.to_string();
        assert!(s.contains("~ mode"), "{s}");
        assert!(
            s.contains("rule -> global") || s.contains("\"rule\" -> \"global\""),
            "{s}"
        );
    }

    #[test]
    fn a_wholesale_list_change_still_shows_up() {
        // Mixed shapes cannot be diffed by name or as a set, so the whole list
        // is reported rather than silently ignored.
        let d = diff(
            &json!({"x": [{"a": 1}, "plain"]}),
            &json!({"x": [{"a": 2}, "plain"]}),
        );
        assert_eq!(d.len(), 1);
        assert!(matches!(d.entries[0].change, Change::Changed { .. }));
    }
}
