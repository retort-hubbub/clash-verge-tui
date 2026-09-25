//! Deep merging of configuration documents.
//!
//! An override is expressed as a *patch document* that is merged into a base
//! configuration. The rules are intentionally simple, because a merge engine
//! that surprises its user is worse than no merge engine at all:
//!
//! | Base | Patch | Result |
//! |---|---|---|
//! | mapping | mapping | merged recursively, key by key |
//! | list | list | per [`ArrayStrategy`] |
//! | anything | `null` | the key is **deleted** |
//! | anything else | anything else | patch wins |
//!
//! Deleting via `null` is the one convention worth calling out: YAML has no
//! other way to express "remove this key", and the alternative — a separate
//! `remove:` block — splits related edits across two places.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

/// How two lists are combined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ArrayStrategy {
    /// The patch list wins outright. Least surprising, and the default.
    #[default]
    Replace,
    /// Base elements first, then the patch's.
    Append,
    /// Patch elements first, then the base's. Use this to give your own rules
    /// precedence over a subscription's.
    Prepend,
    /// Like [`Self::Append`], but elements already present are not repeated.
    /// O(n·m) with a structural comparison; fine for the list sizes here.
    Union,
}

impl ArrayStrategy {
    /// Apply the strategy to two lists, returning the combined list.
    #[must_use]
    pub fn apply(self, base: &[Value], patch: &[Value]) -> Vec<Value> {
        match self {
            Self::Replace => patch.to_vec(),
            Self::Append => {
                let mut out = base.to_vec();
                out.extend(patch.iter().cloned());
                out
            }
            Self::Prepend => {
                let mut out = patch.to_vec();
                out.extend(base.iter().cloned());
                out
            }
            Self::Union => {
                let mut out = base.to_vec();
                for item in patch {
                    if !out.contains(item) {
                        out.push(item.clone());
                    }
                }
                out
            }
        }
    }
}

/// Array handling for a merge.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MergeOptions {
    /// Strategy used for any list not named in `per_key`.
    pub default_array: ArrayStrategy,
    /// Strategy for specific *top-level* keys, e.g. `rules`.
    pub per_key: BTreeMap<String, ArrayStrategy>,
}

impl MergeOptions {
    /// Replace every list. The default, and what a user writing a plain YAML
    /// patch expects.
    #[must_use]
    pub fn replace() -> Self {
        Self::default()
    }

    /// Append to `rules`, `proxies` and `proxy-groups`, replace everything else.
    ///
    /// This is the shape people mean by "add my own rules on top of the
    /// subscription". Lists appear at more than one depth, so a `per_key` entry
    /// also applies to a nested key with the same name, which is what makes
    /// `rules` work inside `dns`-adjacent sub-objects too.
    #[must_use]
    pub fn extend_lists() -> Self {
        let mut per_key = BTreeMap::new();
        for key in [
            "rules",
            "proxies",
            "proxy-groups",
            "rule-providers",
            "proxy-providers",
        ] {
            per_key.insert(key.to_owned(), ArrayStrategy::Append);
        }
        Self {
            default_array: ArrayStrategy::Replace,
            per_key,
        }
    }

    /// Prepend to the list-shaped keys, so the patch outranks the base.
    #[must_use]
    pub fn prepend_lists() -> Self {
        let mut per_key = BTreeMap::new();
        for key in ["rules", "proxy-groups"] {
            per_key.insert(key.to_owned(), ArrayStrategy::Prepend);
        }
        Self {
            default_array: ArrayStrategy::Replace,
            per_key,
        }
    }

    /// Strategy to use when merging the mapping at `key`.
    #[must_use]
    pub fn strategy_for(&self, key: &str) -> ArrayStrategy {
        self.per_key.get(key).copied().unwrap_or(self.default_array)
    }

    /// Override the strategy for one key.
    pub fn set(&mut self, key: impl Into<String>, strategy: ArrayStrategy) -> &mut Self {
        self.per_key.insert(key.into(), strategy);
        self
    }
}

/// Merge `patch` into `base` in place.
///
/// # Panics
/// Never panics; a patch that cannot be merged as requested simply overwrites.
pub fn deep_merge(base: &mut Value, patch: &Value, options: &MergeOptions) {
    merge_at(base, patch, options, None);
}

/// Merge `patch` into `base`, using `key`'s strategy for array decisions.
fn merge_at(base: &mut Value, patch: &Value, options: &MergeOptions, key: Option<&str>) {
    match (base, patch) {
        (Value::Object(base_map), Value::Object(patch_map)) => {
            for (k, patch_value) in patch_map {
                if patch_value.is_null() {
                    // Explicit null deletes.
                    base_map.remove(k);
                    continue;
                }
                match base_map.get_mut(k) {
                    Some(slot) => merge_at(slot, patch_value, options, Some(k)),
                    None => {
                        // A brand new subtree is inserted as it stands, without
                        // recursing into it — the parent's array strategy would
                        // corrupt a nested list that happens to share a name
                        // with a top-level key. The nulls come out first,
                        // though: `{new: {a: null}}` asks for a `new` with no
                        // `a`, and inserting it verbatim produced a `new` that
                        // *held* a null, which is not what the same patch means
                        // when `new` already exists.
                        base_map.insert(k.clone(), without_deletions(patch_value));
                    }
                }
            }
        }
        (Value::Array(base_arr), Value::Array(patch_arr)) => {
            let strategy = key.map_or(options.default_array, |k| options.strategy_for(k));
            // The patch's nulls are removed before the strategy sees them, so
            // that "a null means nothing is there" holds for a list the base
            // already has as well as for one this patch creates. A strategy
            // that appended them would otherwise introduce an element that no
            // configuration wants and the validator rejects.
            let additions = without_deletions_in(patch_arr);
            *base_arr = strategy.apply(base_arr, &additions);
        }
        (slot, patch_value) => {
            // A shape the base cannot absorb is replaced outright — and the
            // replacement gets the same treatment, because a null four levels
            // inside it means the same thing as a null at the top.
            *slot = without_deletions(patch_value);
        }
    }
}

/// Remove the nulls a patch uses to say "nothing here".
///
/// The reading is the same wherever the null sits, which is the point: a null
/// as a key's value means the key is not there, and a null as a list's element
/// means the element is not there. Leaving the latter in place produced list
/// items no configuration wants — and the two cases then disagreed, because a
/// subtree being created kept its nulls while a subtree being merged did not.
fn without_deletions(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| (k.clone(), without_deletions(v)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(without_deletions_in(items)),
        other => other.clone(),
    }
}

/// [`without_deletions`] for a list: the null elements go, the rest is cleaned.
fn without_deletions_in(items: &[Value]) -> Vec<Value> {
    items
        .iter()
        .filter(|v| !v.is_null())
        .map(without_deletions)
        .collect()
}

/// Merge without mutating, returning a new document.
#[must_use]
pub fn merged(base: &Value, patch: &Value, options: &MergeOptions) -> Value {
    let mut out = base.clone();
    deep_merge(&mut out, patch, options);
    out
}

/// Apply a sequence of patches in order.
#[must_use]
pub fn merged_all(base: &Value, patches: &[Value], options: &MergeOptions) -> Value {
    let mut out = base.clone();
    for patch in patches {
        deep_merge(&mut out, patch, options);
    }
    out
}

/// The six list keys `clash-verge-rev` merge profiles use as directives.
///
/// Rather than making users memorise them, [`expand_directives`] rewrites them
/// into an ordinary patch plus per-key strategies, so one code path handles
/// both styles.
pub const DIRECTIVES: &[(&str, ArrayStrategy)] = &[
    ("prepend-rules", ArrayStrategy::Prepend),
    ("append-rules", ArrayStrategy::Append),
    ("prepend-proxies", ArrayStrategy::Prepend),
    ("append-proxies", ArrayStrategy::Append),
    ("prepend-proxy-groups", ArrayStrategy::Prepend),
    ("append-proxy-groups", ArrayStrategy::Append),
];

/// A `prepend-`/`append-` key this build has no entry for, if there is one.
///
/// A directive somebody wrote expecting it to work. Left in place it becomes a
/// top-level key of the generated document, which the core loads without
/// complaint — so the merge silently does nothing and nothing anywhere says so.
#[must_use]
pub fn unknown_directive(patch: &Value) -> Option<String> {
    patch.as_object()?.keys().find_map(|key| {
        ((key.starts_with("prepend-") || key.starts_with("append-"))
            && !DIRECTIVES.iter().any(|(directive, _)| directive == key))
        .then(|| key.clone())
    })
}

/// Rewrite `prepend-*`/`append-*` directives into a plain patch.
///
/// A merge profile written either way — or mixing both — produces the same
/// result:
///
/// ```yaml
/// # clash-verge-rev style
/// prepend-rules: [DOMAIN-SUFFIX,corp.example,DIRECT]
///
/// # equivalent, written the clash-verge-tui way
/// rules:
///   - DOMAIN-SUFFIX,corp.example,DIRECT
/// ```
///
/// Returns the patch to merge and the options that reproduce the directive
/// semantics. Directives take precedence over a plain key of the same name in
/// the same document.
#[must_use]
pub fn expand_directives(patch: &Value, options: &MergeOptions) -> (Value, MergeOptions) {
    let Some(map) = patch.as_object() else {
        return (patch.clone(), options.clone());
    };

    let mut out: Map<String, Value> = map.clone();
    let mut opts = options.clone();
    for (directive, strategy) in DIRECTIVES {
        let Some(value) = out.remove(*directive) else {
            continue;
        };
        let target = directive
            .strip_prefix("prepend-")
            .or_else(|| directive.strip_prefix("append-"))
            .unwrap_or(directive);
        opts.set(target, *strategy);
        // A directive and a plain key in the same document are merged together;
        // the directive's strategy decides where its items land.
        match out.get_mut(target) {
            Some(Value::Array(existing)) => {
                let mut combined =
                    ArrayStrategy::Union.apply(existing, ensure_array(&value).as_slice());
                if combined.is_empty() {
                    combined = ensure_array(&value);
                }
                *existing = combined;
            }
            _ => {
                out.insert(target.to_owned(), value);
            }
        }
    }
    (Value::Object(out), opts)
}

fn ensure_array(v: &Value) -> Vec<Value> {
    match v {
        Value::Array(a) => a.clone(),
        other => vec![other.clone()],
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[allow(clippy::needless_pass_by_value)] // a by-value helper keeps the tests terse
    fn m(base: Value, patch: Value) -> Value {
        merged(&base, &patch, &MergeOptions::default())
    }

    #[test]
    fn merges_nested_mappings_key_by_key() {
        let base = json!({"dns": {"enable": false, "nameserver": ["1.1.1.1"]}, "mode": "rule"});
        let patch = json!({"dns": {"enable": true}});
        let out = m(base, patch);
        assert_eq!(out["dns"]["enable"], json!(true));
        assert_eq!(
            out["dns"]["nameserver"],
            json!(["1.1.1.1"]),
            "sibling keys survive"
        );
        assert_eq!(out["mode"], json!("rule"), "untouched keys survive");
    }

    #[test]
    fn adds_new_keys_without_touching_siblings() {
        let out = m(json!({"a": 1}), json!({"b": {"c": 2}}));
        assert_eq!(out, json!({"a": 1, "b": {"c": 2}}));
    }

    #[test]
    fn null_deletes_a_key() {
        let out = m(json!({"a": 1, "b": 2}), json!({"a": null}));
        assert_eq!(out, json!({"b": 2}));
    }

    #[test]
    fn replaces_lists_by_default() {
        let out = m(json!({"rules": ["a", "b"]}), json!({"rules": ["c"]}));
        assert_eq!(out["rules"], json!(["c"]));
    }

    #[test]
    fn append_and_prepend_respect_order() {
        let mut o = MergeOptions::default();
        o.set("rules", ArrayStrategy::Append);
        let out = merged(&json!({"rules": ["base"]}), &json!({"rules": ["mine"]}), &o);
        assert_eq!(out["rules"], json!(["base", "mine"]));

        let mut o = MergeOptions::default();
        o.set("rules", ArrayStrategy::Prepend);
        let out = merged(&json!({"rules": ["base"]}), &json!({"rules": ["mine"]}), &o);
        assert_eq!(out["rules"], json!(["mine", "base"]));
    }

    #[test]
    fn union_deduplicates_structurally() {
        let mut o = MergeOptions::default();
        o.set("rules", ArrayStrategy::Union);
        let out = merged(
            &json!({"rules": ["a", "b"]}),
            &json!({"rules": ["b", "c", "b"]}),
            &o,
        );
        assert_eq!(out["rules"], json!(["a", "b", "c"]));

        let objs = merged(
            &json!({"p": [{"name": "x"}]}),
            &json!({"p": [{"name": "x"}, {"name": "y"}]}),
            &MergeOptions {
                default_array: ArrayStrategy::Union,
                ..Default::default()
            },
        );
        assert_eq!(objs["p"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn scalars_and_type_changes_overwrite() {
        assert_eq!(m(json!({"a": 1}), json!({"a": "text"}))["a"], json!("text"));
        assert_eq!(
            m(json!({"a": {"b": 1}}), json!({"a": [1]}))["a"],
            json!([1])
        );
        assert_eq!(
            m(json!({"a": [1]}), json!({"a": {"b": 1}}))["a"],
            json!({"b": 1})
        );
    }

    #[test]
    fn a_top_level_strategy_does_not_leak_into_fresh_subtrees() {
        // `dns.rule-providers` is a new subtree; the top-level `rule-providers`
        // strategy must not turn its mapping into something else.
        let mut o = MergeOptions::default();
        o.set("rules", ArrayStrategy::Append);
        let out = merged(&json!({}), &json!({"nested": {"rules": ["only"]}}), &o);
        assert_eq!(out["nested"]["rules"], json!(["only"]));
    }

    #[test]
    fn per_key_strategy_applies_at_depth_too() {
        let mut o = MergeOptions::default();
        o.set("rules", ArrayStrategy::Append);
        let out = merged(
            &json!({"a": {"rules": ["x"]}}),
            &json!({"a": {"rules": ["y"]}}),
            &o,
        );
        assert_eq!(out["a"]["rules"], json!(["x", "y"]));
    }

    #[test]
    fn extend_lists_targets_the_expected_keys() {
        let o = MergeOptions::extend_lists();
        assert_eq!(o.strategy_for("rules"), ArrayStrategy::Append);
        assert_eq!(o.strategy_for("proxies"), ArrayStrategy::Append);
        assert_eq!(o.strategy_for("nameserver"), ArrayStrategy::Replace);
    }

    #[test]
    fn directives_expand_into_a_plain_patch() {
        let patch = json!({
            "prepend-rules": ["DOMAIN-SUFFIX,corp.example,DIRECT"],
            "append-proxies": [{"name": "extra", "type": "socks", "server": "1.2.3.4", "port": 1}],
            "mode": "global"
        });
        let (expanded, opts) = expand_directives(&patch, &MergeOptions::default());
        assert!(
            expanded.get("prepend-rules").is_none(),
            "directive must be consumed"
        );
        assert_eq!(expanded["mode"], json!("global"));
        assert_eq!(opts.strategy_for("rules"), ArrayStrategy::Prepend);
        assert_eq!(opts.strategy_for("proxies"), ArrayStrategy::Append);

        let out = merged(
            &json!({"rules": ["MATCH,DIRECT"], "proxies": []}),
            &expanded,
            &opts,
        );
        assert_eq!(
            out["rules"],
            json!(["DOMAIN-SUFFIX,corp.example,DIRECT", "MATCH,DIRECT"])
        );
        assert_eq!(out["proxies"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn directives_coexist_with_a_plain_key_of_the_same_name() {
        let patch = json!({
            "rules": ["MATCH,DIRECT"],
            "prepend-rules": ["DOMAIN-SUFFIX,corp.example,DIRECT"]
        });
        let (expanded, opts) = expand_directives(&patch, &MergeOptions::default());
        let out = merged(&json!({"rules": ["SUB,REJECT"]}), &expanded, &opts);
        let rules: Vec<&str> = out["rules"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(rules.contains(&"DOMAIN-SUFFIX,corp.example,DIRECT"));
        assert!(rules.contains(&"MATCH,DIRECT"));
        assert!(rules.contains(&"SUB,REJECT"));
        assert_eq!(rules.len(), 3, "no duplicates: {rules:?}");
    }

    #[test]
    fn merged_all_applies_patches_in_order() {
        let out = merged_all(
            &json!({"a": 1}),
            &[json!({"a": 2}), json!({"a": 3})],
            &MergeOptions::default(),
        );
        assert_eq!(out["a"], json!(3));
    }
}
