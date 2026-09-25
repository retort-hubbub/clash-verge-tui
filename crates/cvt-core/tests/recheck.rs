//! A second verifier's re-check of the eighteen findings in
//! `docs/VERIFICATION-REPORT.md`.
//!
//! This file is written from the *claims* the author makes, not from the tests
//! they wrote for them. Where a claim is behavioural over a space of inputs, a
//! generated attack is used rather than the one input the author happened to
//! pick; where a claim is about the core's own behaviour, mihomo `v1.19.31`'s
//! source is quoted.
//!
//! Tests whose name starts with a finding id (`f3_`, `f4_`, …) check that
//! finding. A test that is expected to **fail** carries a `FAILING:` marker in
//! its doc comment, states the minimal input, and asserts the behaviour the
//! claim promises — so a run of this file shows at a glance how many of the
//! author's claims survive.
//!
//! Nothing here starts or stops the mihomo core.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::too_many_lines
)]

use std::collections::BTreeMap;
use std::path::{Path as StdPath, PathBuf};
use std::sync::{Arc, Mutex};

use cvt_core::enhance::diff::{Change, DEFAULT_LIMIT, diff, diff_limited};
use cvt_core::enhance::merge::{ArrayStrategy, MergeOptions, deep_merge, merged};
use cvt_core::enhance::overlay::Overlay;
use cvt_core::enhance::path::{self, Path, Segment};
use cvt_core::enhance::pipeline::Pipeline;
use cvt_core::mihomo::client::Client;
use cvt_core::mihomo::endpoint::Endpoint;
use cvt_core::mihomo::types::{
    Connection, GroupsResponse, Metadata, ProxiesResponse, ProxyProvidersResponse,
    RuleProviderInfo, RuleProvidersResponse, RulesResponse,
};
use cvt_core::model::config::Config;
use cvt_core::model::rule::Rule;
use cvt_core::profile::item::{PrfItem, ProfileType, SeqPatch};
use cvt_core::profile::store::ProfileStore;
use cvt_core::{AppPaths, Error, ReloadMode, Service, validate};
use proptest::prelude::*;
use proptest::test_runner::Config as ProptestConfig;
use serde_json::{Map, Value, json};
use tempfile::TempDir;

// ------------------------------------------------------------------ helpers

/// A document that passes `validate` and that the pipeline can generate from.
const BASE: &str = r"
mixed-port: 7890
external-controller: 127.0.0.1:9090
mode: rule
proxies:
  - { name: JP 01, type: vless, server: 1.2.3.4, port: 443, uuid: u }
proxy-groups:
  - { name: PROXY, type: select, proxies: [JP 01, DIRECT] }
rules:
  - DOMAIN-SUFFIX,google.com,PROXY
  - MATCH,DIRECT
";

fn config(text: &str) -> Config {
    Config::from_yaml(text).unwrap_or_else(|e| panic!("the config must parse: {e}\n{text}"))
}

/// Every *error* a config produces, as `(code, message)`.
fn errors(c: &Config) -> Vec<(&'static str, String)> {
    validate::check(c)
        .errors_iter()
        .map(|d| (d.code, d.message.clone()))
        .collect()
}

fn error_codes(c: &Config) -> Vec<&'static str> {
    errors(c).into_iter().map(|(c, _)| c).collect()
}

/// Every diagnostic code, warnings included.
fn all_codes(c: &Config) -> Vec<&'static str> {
    validate::check(c)
        .diagnostics
        .iter()
        .map(|d| d.code)
        .collect()
}

fn temp_store() -> (TempDir, ProfileStore) {
    let dir = TempDir::new().unwrap();
    let paths = AppPaths::new(dir.path());
    paths.ensure_dirs().unwrap();
    let store = ProfileStore::load(&paths).unwrap();
    (dir, store)
}

/// The identity `profile/item.rs` documents: a named mapping is the same item
/// when its `name` matches, and two anonymous values are the same item when
/// they are structurally equal.
fn same_item(a: &Value, b: &Value) -> bool {
    let name = |v: &Value| {
        v.get("name")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| v.as_str().map(str::to_owned))
    };
    match (name(a), name(b)) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

fn same_identity_anywhere(needle: &Value, list: &[Value]) -> usize {
    list.iter().filter(|v| same_item(v, needle)).count()
}

/// A patch's `null` means "nothing is there". This is the path of every such
/// position, expressed as a sequence of object keys and array indices.
fn null_positions(patch: &Value, at: Vec<String>, out: &mut Vec<Vec<String>>) {
    match patch {
        Value::Null => out.push(at),
        Value::Object(map) => {
            for (k, v) in map {
                let mut next = at.clone();
                next.push(k.clone());
                null_positions(v, next, out);
            }
        }
        Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                let mut next = at.clone();
                next.push(i.to_string());
                null_positions(v, next, out);
            }
        }
        _ => {}
    }
}

fn resolve_path<'a>(doc: &'a Value, path: &[String]) -> Option<&'a Value> {
    let mut cur = doc;
    for step in path {
        cur = match cur {
            Value::Object(map) => map.get(step)?,
            Value::Array(items) => items.get(step.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(cur)
}

// ============================================================== finding F1
// `validate` must not call the built-in `GLOBAL`/`PASS-RULE` policies dangling.

/// The seven policies mihomo always provides, from `GET /proxies` on a real
/// `v1.19.31` core (spec §4.1). Each must be usable both as a rule target and
/// as a group member, and the *same* check must still fire for a typo — a
/// validator that stopped reporting anything would also pass the first half.
#[test]
fn f1_the_built_in_policies_are_known_in_rule_targets_and_group_members() {
    const BUILTINS: [&str; 7] = [
        "DIRECT",
        "REJECT",
        "REJECT-DROP",
        "PASS",
        "PASS-RULE",
        "COMPATIBLE",
        "GLOBAL",
    ];
    for b in BUILTINS {
        let text = format!(
            "mixed-port: 7890\n\
             external-controller: 127.0.0.1:9090\n\
             proxy-groups:\n  - {{ name: G, type: select, proxies: [{b}] }}\n\
             rules:\n  - DOMAIN-SUFFIX,x.example,{b}\n  - MATCH,DIRECT\n"
        );
        let errs = errors(&config(&text));
        assert!(
            errs.is_empty(),
            "`{b}` is built in, but validate reported {errs:?}"
        );
    }

    // Control: a name that is *not* built in still produces both codes, so the
    // assertions above are not vacuous.
    let typo = config(
        "mixed-port: 7890\n\
         proxy-groups:\n  - { name: G, type: select, proxies: [GHOST] }\n\
         rules:\n  - DOMAIN-SUFFIX,x.example,GHOST\n  - MATCH,DIRECT\n",
    );
    let codes = error_codes(&typo);
    assert!(codes.contains(&"E-DANGLING-POLICY"), "{codes:?}");
    assert!(codes.contains(&"E-DANGLING-GROUP-MEMBER"), "{codes:?}");
}

/// A relaxation the F1 fix inherited: `is_known_policy` compares the built-in
/// names *case-insensitively*, while mihomo looks the target up in a map keyed
/// by the exact names. `config/config.go` at v1.19.31:
///
/// ```text
/// tp, payload, target, params := RC.ParseRulePayload(line, true)
/// if _, ok := proxies[target]; !ok {
///     return nil, fmt.Errorf("%s[%d] [%s] error: proxy [%s] not found", ...)
/// }
/// ```
///
/// and `ParseRulePayload` upper-cases the *rule type* only, so `direct` is not
/// a key of that map.
///
/// The recheck asked for a live core to settle it, so here is the live core.
/// mihomo v1.19.31, `verge-mihomo -t -f <file>`:
///
/// ```text
/// rules[0] [MATCH,direct] error: proxy [direct] not found
/// ```
///
/// The validator used to accept it, which is the same family as F11: a
/// configuration reported clean and refused by the core.
#[test]
fn f1_a_case_mismatched_built_in_policy_is_reported() {
    let c = config("mixed-port: 7890\nrules:\n  - MATCH,direct\n");
    assert!(
        error_codes(&c).contains(&"E-DANGLING-POLICY"),
        "`MATCH,direct` is refused by the core but the validator said {:?}",
        error_codes(&c)
    );
    // And the exact spelling is still fine, so the check is not simply strict.
    let ok = config("mixed-port: 7890\nrules:\n  - MATCH,DIRECT\n");
    assert!(!error_codes(&ok).contains(&"E-DANGLING-POLICY"));
}

// ============================================================== finding F2
// (fixed during the earlier audit; kept as an independent control)

#[test]
fn f2_an_index_of_i64_min_is_reported_rather_than_panicking() {
    let doc = json!({"proxies": [{"name": "A"}]});
    let p = Path::parse("proxies[-9223372036854775808]").unwrap();
    assert!(path::get(&doc, &p).is_none());
    let mut target = doc.clone();
    assert!(path::push(&mut target, &p, json!("x")).is_err());
    assert!(path::set(&mut target, &p, json!("x")).is_err());
    assert!(path::remove(&mut target, &p).unwrap().is_none());
    assert_eq!(target, doc);
}

// ============================================================== finding F3
// `path::push` must be atomic: an `Err` leaves the document byte-identical.

/// The shapes the finding's own reproduction did *not* use, chosen so that the
/// read-only walk and the materialising descent have something to disagree
/// about: nulls in the middle of the path, selectors, negative indices, list
/// elements that are scalars, and a leaf that exists as a scalar.
fn push_shapes() -> Vec<(Value, &'static str)> {
    vec![
        (json!({}), "a[0]"),
        (json!({}), "a.b[0]"),
        (json!({}), "x.y.z[0]"),
        (json!({"a": {}}), "a.b[0]"),
        (json!({"a": []}), "a[0].b"),
        (json!({"a": [null]}), "a[0].b[0]"),
        (json!({"a": [null]}), "a[0]"),
        (json!({"a": [{"b": 1}]}), "a[name=x].b"),
        (json!({"a": [{"b": 5}]}), "a[0].b.c"),
        (json!({"a": 1}), "a.b"),
        (json!({"a": 1}), "a"),
        (json!({"a": null}), "a.b[0]"),
        (json!({"a": {"b": [{"c": 1}]}}), "a.b[0].c.d[0]"),
        (json!([]), "rules"),
        (json!(5), "rules"),
        (json!(null), "a.b[0]"),
        (json!({"a": [{"name": "x"}]}), "a[1]"),
        (json!({"a": [{"name": "x"}]}), "a[name=y]"),
        (json!({"a": {"b": {"c": 1}}}), "a.b.c.d"),
        (json!({"a": [{}]}), "a[-1].b[0]"),
        (json!({"a": "text"}), "a[0]"),
    ]
}

#[test]
fn f3_a_failed_push_leaves_every_path_shape_alone() {
    let mut failures = 0;
    for (doc, text) in push_shapes() {
        let p = Path::parse(text).unwrap_or_else(|e| panic!("{text} should parse: {e}"));
        let mut target = doc.clone();
        if path::push(&mut target, &p, json!("pushed")).is_err() {
            failures += 1;
            assert_eq!(
                target, doc,
                "push({doc}) via `{text}` failed and changed the document"
            );
        } else {
            // Not vacuous in the other direction either: a push that reports
            // success must have put the item at the end of the addressed list.
            let list = path::get(&target, &p)
                .and_then(Value::as_array)
                .unwrap_or_else(|| panic!("`{text}` reported success but is not a list"));
            assert_eq!(
                list.last(),
                Some(&json!("pushed")),
                "`{text}` reported success but did not append"
            );
        }
    }
    assert!(failures > 0, "no shape was refused, so nothing was checked");
}

/// The algorithm `push` used before `bdff828`: descend with materialisation,
/// and only then discover that the leaf is not a list. Re-implemented here so
/// that the property above can be *seen* to be capable of failing — a test
/// that cannot fail is not evidence.
fn push_the_old_way(root: &mut Value, p: &Path, item: Value) -> Result<(), ()> {
    fn descend<'a>(cur: &'a mut Value, seg: &Segment) -> Option<&'a mut Value> {
        if cur.is_null() {
            *cur = Value::Object(Map::new());
        }
        match seg {
            Segment::Key(k) => cur
                .as_object_mut()?
                .entry(k.clone())
                .or_insert(Value::Null)
                .into(),
            Segment::Index(i) => {
                let len = cur.as_array()?.len();
                let idx = if *i < 0 {
                    len.checked_sub(usize::try_from(i.unsigned_abs()).ok()?)?
                } else {
                    usize::try_from(*i).ok().filter(|n| *n < len)?
                };
                cur.as_array_mut()?.get_mut(idx)
            }
            Segment::Selector { key, value } => cur
                .as_array_mut()?
                .iter_mut()
                .find(|e| e.get(key.as_str()).and_then(Value::as_str) == Some(value.as_str())),
        }
    }

    let (last, parents) = p.segments().split_last().ok_or(())?;
    let mut cur = root;
    for seg in parents {
        cur = descend(cur, seg).ok_or(())?;
    }
    let Segment::Key(leaf) = last else {
        return Err(());
    };
    if cur.is_null() {
        *cur = Value::Object(Map::new());
    }
    let obj = cur.as_object_mut().ok_or(())?;
    let entry = obj
        .entry(leaf.clone())
        .or_insert_with(|| Value::Array(Vec::new()));
    if entry.is_null() {
        *entry = Value::Array(Vec::new());
    }
    entry.as_array_mut().ok_or(())?.push(item);
    Ok(())
}

#[test]
fn f3_the_property_fails_for_the_algorithm_the_fix_replaced() {
    let (doc, text) = (json!({}), "x.y.z[0]");
    let p = Path::parse(text).unwrap();
    let mut old = doc.clone();
    assert!(push_the_old_way(&mut old, &p, json!("v")).is_err());
    assert_ne!(
        old, doc,
        "the pre-fix algorithm was atomic after all, so the tests above prove nothing"
    );
    assert_eq!(old, json!({"x": {"y": {"z": null}}}));

    // ... and the real `push` is not.
    let mut now = doc.clone();
    assert!(path::push(&mut now, &p, json!("v")).is_err());
    assert_eq!(now, doc);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4000))]

    /// The same claim over documents and paths built from a different grammar
    /// than the author's generator: every document may hold nulls, nested
    /// arrays of mappings, and out-of-range indices, and every path may end in
    /// a key, an index or a selector.
    #[test]
    fn f3_push_is_atomic_over_generated_documents_and_paths(
        doc in arb_document(),
        path_text in arb_path_text(),
        item in arb_value(),
    ) {
        let Ok(p) = Path::parse(&path_text) else {
            return Ok(());
        };
        let mut target = doc.clone();
        if path::push(&mut target, &p, item).is_err() {
            prop_assert_eq!(
            target,
            doc,
            "a failed push changed the document (path {:?})",
            path_text
        ); } else {
            let list = path::get(&target, &p).and_then(Value::as_array);
            prop_assert!(
                list.is_some_and(|l| !l.is_empty()),
                "a push that reported success produced no list at {:?}",
                path_text
            );
        }
    }
}

// The generator. Small, null-heavy documents, so that the paths have something
// to walk into, plus paths that mix every segment kind.
fn arb_key() -> impl Strategy<Value = String> {
    prop_oneof![
        prop::sample::select(vec![
            "a", "b", "c", "rules", "proxies", "dns", "", "my.key", "a[b]",
        ])
        .prop_map(str::to_owned),
        "[a-c.]{0,4}",
    ]
}

fn arb_scalar_value() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(|i| json!(i)),
        prop::sample::select(vec!["", "x", "MATCH,DIRECT", "a b", "日本"])
            .prop_map(|s| Value::String(s.to_owned())),
    ]
}

fn arb_value() -> impl Strategy<Value = Value> {
    arb_scalar_value().prop_recursive(3, 16, 4, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..3).prop_map(Value::Array),
            prop::collection::vec((arb_key(), inner), 0..3).prop_map(|pairs| {
                Value::Object(pairs.into_iter().collect::<Map<String, Value>>())
            }),
        ]
    })
}

fn arb_document() -> impl Strategy<Value = Value> {
    prop::collection::vec((arb_key(), arb_value()), 0..4)
        .prop_map(|pairs| Value::Object(pairs.into_iter().collect::<Map<String, Value>>()))
}

fn arb_path_text() -> impl Strategy<Value = String> {
    let segment = prop_oneof![
        3 => arb_key(),
        2 => prop::sample::select(vec![
            "[-1]", "[0]", "[1]", "[2]", "[-9223372036854775808]", "[9223372036854775807]"
        ])
        .prop_map(str::to_owned),
        2 => arb_key().prop_map(|k| format!("[{k}=x]")),
        1 => arb_key().prop_map(|k| format!("[{k}=]")),
    ];
    prop::collection::vec(segment, 1..4).prop_map(|segments| {
        let mut out = String::new();
        for (i, s) in segments.iter().enumerate() {
            if !s.starts_with('[') && i > 0 {
                out.push('.');
            }
            out.push_str(s);
        }
        out
    })
}

// ============================================================== finding F4
// A `null` in a patch means "nothing is there" — as a key's value in an
// existing *and* in a brand-new subtree, and as a list element.

#[test]
fn f4_a_null_means_nothing_is_there_in_the_three_positions_the_claim_names() {
    let plain = MergeOptions::default();
    let cases: Vec<(Value, Value, Value, &str)> = vec![
        // (base, patch, expected, why)
        (
            json!({}),
            json!({"new": {"a": null}}),
            json!({"new": {}}),
            "null in a brand-new subtree",
        ),
        (
            json!({"new": {}}),
            json!({"new": {"a": null}}),
            json!({"new": {}}),
            "the same null with the subtree already present",
        ),
        (
            json!({"a": {"b": 1}}),
            json!({"a": {"b": null}}),
            json!({"a": {}}),
            "null deletes a key of an existing subtree",
        ),
        (
            json!({"a": 1}),
            json!({"a": {"b": null}}),
            json!({"a": {}}),
            "a scalar replaced by a subtree holding only a null",
        ),
        (
            json!({"a": [1]}),
            json!({"a": {"b": null}}),
            json!({"a": {}}),
            "a list replaced by a subtree holding only a null",
        ),
        (
            json!({}),
            json!({"a": {"b": {"c": null}}}),
            json!({"a": {"b": {}}}),
            "a null four levels down a brand-new subtree",
        ),
        (
            json!({}),
            json!({"a": [1, null, 2]}),
            json!({"a": [1, 2]}),
            "null as a list element of a new list",
        ),
        (
            json!({"a": [7, 8]}),
            json!({"a": [1, null, 2]}),
            json!({"a": [1, 2]}),
            "null as a list element with Replace",
        ),
        (
            json!({"a": [null]}),
            json!({"a": null}),
            json!({}),
            "a null patch value deletes the key that held a null",
        ),
        (
            json!({}),
            json!({"a": {"b": [null, {"c": null}]}}),
            json!({"a": {"b": [{}]}}),
            "nulls inside a list inside a new subtree",
        ),
    ];
    for (base, patch, expected, why) in cases {
        let out = merged(&base, &patch, &plain);
        assert_eq!(out, expected, "{why}: merged({base}, {patch})");
        // ... and the same patch applied again changes nothing.
        assert_eq!(
            merged(&out, &patch, &plain),
            out,
            "{why}: the merge is not idempotent"
        );
    }

    // A strategy that appends is the case a list-element null could sneak past.
    let mut append = MergeOptions::default();
    append.set("rules", ArrayStrategy::Append);
    assert_eq!(
        merged(
            &json!({"rules": ["x"]}),
            &json!({"rules": ["y", null]}),
            &append
        ),
        json!({"rules": ["x", "y"]}),
        "Append must not introduce the patch's null element"
    );
    let mut union = MergeOptions::default();
    union.set("rules", ArrayStrategy::Union);
    assert_eq!(
        merged(
            &json!({"rules": ["x"]}),
            &json!({"rules": [null, "z"]}),
            &union
        ),
        json!({"rules": ["x", "z"]}),
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(3000))]

    /// Generated patches with nulls in arbitrary positions, against
    /// null-free bases (so any null left in the result came from the patch).
    #[test]
    fn f4_no_null_the_patch_declares_survives_the_merge(
        base in arb_null_free_value(),
        patch in arb_value(),
    ) {
        // A patch that *is* a null has no key to delete: `deep_merge` replaces
        // the whole document with it. That case is pinned on its own by
        // `f4_a_patch_that_is_itself_null_replaces_the_document`; the claim
        // under test here is about nulls *inside* a patch.
        prop_assume!(!patch.is_null());
        let options = MergeOptions::default();
        let out = merged(&base, &patch, &options);

        let mut positions = Vec::new();
        null_positions(&patch, Vec::new(), &mut positions);
        for at in &positions {
            if let Some(v) = resolve_path(&out, at) {
                prop_assert!(
                    !v.is_null(),
                    "the patch's null at {:?} survived as a value: out={out}, patch={patch}, base={base}",
                    at
                );
            }
        }

        prop_assert_eq!(
            merged(&out, &patch, &options),
            out,
            "merging the same patch twice differs: base={}, patch={}",
            base,
            patch
        );
    }
}

// A null-free value: the base must not contribute nulls of its own, so that
// any null in the output is the patch's responsibility.
fn arb_null_free_value() -> impl Strategy<Value = Value> {
    arb_null_free_scalar().prop_recursive(3, 16, 4, |inner: BoxedStrategy<Value>| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..3).prop_map(Value::Array),
            prop::collection::vec((arb_key(), inner), 0..3).prop_map(|pairs| {
                Value::Object(pairs.into_iter().collect::<Map<String, Value>>())
            }),
        ]
    })
}

fn arb_null_free_scalar() -> impl Strategy<Value = Value> {
    prop_oneof![
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(|i| json!(i)),
        prop::sample::select(vec!["", "x", "MATCH,DIRECT"])
            .prop_map(|s| Value::String(s.to_owned())),
    ]
}

/// A patch that *is* a null is a different case from a null inside a patch:
/// `deep_merge` replaces the whole document with it. Kept as a test so the
/// behaviour is pinned rather than accidental; see the report for why it is
/// not counted as a counterexample to F4.
#[test]
fn f4_a_patch_that_is_itself_null_replaces_the_document() {
    let mut doc = json!({"a": 1});
    deep_merge(&mut doc, &Value::Null, &MergeOptions::default());
    assert_eq!(doc, Value::Null);
}

/// `SeqPatch`'s entries are strings, but a string that parses as JSON is used
/// as the value, so the literal `"null"` becomes a JSON null in the list —
/// which then reaches the generated configuration as a non-string rule.
/// Low severity (a hand-written `prepend: ["null"]`), recorded because F4's
/// claim says "everywhere".
#[test]
fn f4_a_null_in_a_sequence_patch_list_element_is_inserted_not_dropped() {
    let patch = SeqPatch {
        prepend: vec!["null".to_owned()],
        ..SeqPatch::default()
    };
    let out = patch.apply_values(&[json!("A,DIRECT")]);
    assert_eq!(
        out,
        vec![json!(null), json!("A,DIRECT")],
        "a null element is currently inserted; if F4's `everywhere` is meant \
         literally it should be dropped"
    );
}

// ============================================================== finding F5
// `SeqPatch::prepend` and `apply_values` never introduce a duplicate.

#[test]
fn f5_the_two_counterexamples_the_finding_named_are_gone() {
    // A value named twice in one patch is one value.
    let patch = SeqPatch {
        prepend: vec!["X".into(), "X".into()],
        append: vec!["X".into()],
        delete: vec![],
    };
    assert_eq!(patch.apply(&[]), vec!["X"]);
    assert_eq!(patch.apply(&["B".into()]), vec!["X", "B"]);

    // A patch may not re-add what the base already holds, by either spelling.
    let values = SeqPatch {
        prepend: vec!["A".into()],
        append: vec!["A".into()],
        delete: vec![],
    };
    assert_eq!(
        values.apply_values(&[json!("A"), json!("B")]),
        vec![json!("A"), json!("B")]
    );
    let named = SeqPatch {
        prepend: vec![r#"{"name":"A","port":9}"#.into()],
        append: vec![],
        delete: vec![],
    };
    assert_eq!(
        named.apply_values(&[json!({"name": "A", "port": 1})]),
        vec![json!({"name": "A", "port": 1})],
        "a named mapping is identified by its name, not its value"
    );
    // The cross spelling the finding called out: a plain string equals a named
    // mapping with that name.
    let cross = SeqPatch {
        prepend: vec!["A".into()],
        append: vec![],
        delete: vec![],
    };
    assert_eq!(
        cross.apply_values(&[json!({"name": "A"})]),
        vec![json!({"name": "A"})]
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4000))]

    /// No two items in the output are the same item unless the base already
    /// held two of them: the patch may not introduce a duplicate, whatever
    /// mixture of spellings it uses.
    #[test]
    fn f5_apply_values_never_introduces_a_duplicate(
        base in prop::collection::vec(arb_item(), 0..5),
        prepend in prop::collection::vec(arb_entry(), 0..3),
        append in prop::collection::vec(arb_entry(), 0..3),
        delete in prop::collection::vec(prop::sample::select(vec!["A", "B", "C"]).prop_map(str::to_owned), 0..3),
    ) {
        let patch = SeqPatch { prepend, append, delete };
        let kept: Vec<Value> = base
            .iter()
            .filter(|v| {
                let name = v
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| v.as_str().map(str::to_owned));
                name.is_none_or(|n| !patch.delete.contains(&n))
            })
            .cloned()
            .collect();
        let out = patch.apply_values(&base);
        for i in 0..out.len() {
            for j in (i + 1)..out.len() {
                if same_item(&out[i], &out[j]) {
                    let n = same_identity_anywhere(&out[i], &kept);
                    prop_assert!(
                        n >= 2,
                        "the patch introduced a second {:?}: out={:?}, base kept={:?}, patch={:?}",
                        out[i], out, kept, patch
                    );
                }
            }
        }
    }

    /// The same, for the string flavour.
    #[test]
    fn f5_apply_never_introduces_a_duplicate(
        base in prop::collection::vec("[A-C]{1,2}", 0..5),
        prepend in prop::collection::vec("[A-C]{1,2}", 0..3),
        append in prop::collection::vec("[A-C]{1,2}", 0..3),
        delete in prop::collection::vec("[A-C]{1,2}", 0..3),
    ) {
        let patch = SeqPatch { prepend, append, delete };
        let kept: Vec<&String> = base.iter().filter(|s| !patch.delete.contains(*s)).collect();
        let out = patch.apply(&base);
        for i in 0..out.len() {
            for j in (i + 1)..out.len() {
                if out[i] == out[j] {
                    let n = kept.iter().filter(|s| ***s == out[i]).count();
                    prop_assert!(
                        n >= 2,
                        "the patch introduced a second {:?}: out={:?}, base kept={:?}, patch={:?}",
                        out[i], out, kept, patch
                    );
                }
            }
        }
    }
}

fn arb_item() -> impl Strategy<Value = Value> {
    prop_oneof![
        prop::sample::select(vec!["A", "B", "A,DIRECT"]).prop_map(|s| Value::String(s.to_owned())),
        prop::sample::select(vec!["A", "B", "C"]).prop_map(|n| json!({"name": n, "port": 1})),
        Just(json!({"port": 1})),
        Just(json!({"name": "A", "port": 2})),
    ]
}

fn arb_entry() -> impl Strategy<Value = String> {
    prop_oneof![
        prop::sample::select(vec!["A", "B", "A,DIRECT"]).prop_map(str::to_owned),
        prop::sample::select(vec!["A", "B", "C"]).prop_map(|n| format!(r#"{{"name":"{n}"}}"#)),
        prop::sample::select(vec!["A", "B"]).prop_map(|n| format!(r#"{{"name":"{n}","port":3}}"#)),
        Just(r#"{"port":1}"#.to_owned()),
        Just("null".to_owned()),
    ]
}

// ============================================================== finding F6
// `touched_keys` is complete even when the entry cap truncates the entries.

/// A document with one key that produces far more entries than the cap, and
/// several small keys *after* it — including keys that are not ordinary
/// identifiers and one that is the empty string.
fn many_changes() -> (Value, Value, Vec<String>) {
    let before_rules: Vec<Value> = (0..700).map(|i| json!(format!("R{i},DIRECT"))).collect();
    let after_rules: Vec<Value> = before_rules.iter().take(4).cloned().collect();
    let strange = ["mode", "my.key", "", "a[b]", "x[name=Y]", "proxies[0]"];
    let mut before = Map::new();
    let mut after = Map::new();
    before.insert("rules".to_owned(), Value::Array(before_rules));
    after.insert("rules".to_owned(), Value::Array(after_rules));
    for k in strange {
        before.insert(k.to_owned(), json!({"v": 1}));
        after.insert(k.to_owned(), json!({"v": 2}));
    }
    let mut expected = vec!["rules".to_owned()];
    expected.extend(strange.iter().map(|s| (*s).to_owned()));
    (Value::Object(before), Value::Object(after), expected)
}

#[test]
fn f6_touched_keys_stays_complete_at_every_entry_cap() {
    let (before, after, expected) = many_changes();
    for cap in [0usize, 1, 2, 9, 200, 499, DEFAULT_LIMIT] {
        let d = diff_limited(&before, &after, cap);
        assert_eq!(
            d.touched_keys(),
            expected,
            "cap {cap}: the key summary must not depend on the entry cap"
        );
        if cap < 700 {
            assert!(
                d.entries.len() <= cap,
                "cap {cap}: entries were not truncated ({} entries)",
                d.entries.len()
            );
        }
    }

    // The default `diff` truncates here, and is still complete.
    let d = diff(&before, &after);
    assert!(d.truncated, "700 removals must exceed DEFAULT_LIMIT");
    assert_eq!(d.len(), DEFAULT_LIMIT);
    assert_eq!(d.touched_keys(), expected);

    // A cap of zero reports no entries at all; the *keys* are still known, so
    // `truncated` is the flag a caller has to consult, not `is_empty`.
    let none = diff_limited(&before, &after, 0);
    assert!(none.is_empty() && none.truncated);
    assert_eq!(none.touched_keys(), expected);

    // Control: an unchanged document touches nothing.
    assert!(diff_limited(&before, &before, 1).touched_keys().is_empty());
}

// ============================================================== finding F7
// `ProfileStore::add` reassigns a uid the index already holds.

#[test]
fn f7_add_reassigns_a_uid_in_the_index_and_honours_a_free_one() {
    let (_d, mut store) = temp_store();
    let first = store.add(PrfItem::local("L1", "one"));
    assert_eq!(first, "L1", "a free uid is honoured");

    let second = store.add(PrfItem::local("L1", "two"));
    assert_ne!(second, "L1", "a taken uid must be reassigned");
    assert_eq!(store.items().len(), 2);
    assert_eq!(
        store.get("L1").map(|i| i.name.as_str()),
        Some("one"),
        "the original entry must keep its uid (and not be shadowed)"
    );

    let files: Vec<String> = store.items().iter().map(PrfItem::file_name).collect();
    assert_ne!(files[0], files[1], "two items must not share a document");
    assert_eq!(files[1], format!("{second}.yaml"));

    // Many collisions in a row: every uid stays unique.
    for i in 0..25 {
        let uid = store.add(PrfItem::local("L1", format!("copy {i}")));
        assert_ne!(uid, "L1");
    }
    let uids: std::collections::BTreeSet<&str> =
        store.items().iter().map(|i| i.uid.as_str()).collect();
    assert_eq!(uids.len(), store.items().len(), "uids must stay unique");
}

// ============================================================== finding F8
// `import_from` must not overwrite a document the index does not own.

/// Write a `clash-verge-rev`-shaped source directory: an index and one
/// document. `file:` is what the source index says the document is called,
/// which the module's own comment says is not necessarily `{uid}.yaml`.
fn foreign_installation(root: &StdPath, uid: &str, file: &str, body: &str) {
    std::fs::create_dir_all(root.join("profiles")).unwrap();
    std::fs::write(
        root.join("profiles.yaml"),
        format!("items:\n  - uid: {uid}\n    type: local\n    name: imported\n    file: {file}\n"),
    )
    .unwrap();
    std::fs::write(root.join("profiles").join(file), body).unwrap();
}

#[test]
fn f8_an_orphan_document_named_after_the_uid_is_left_alone() {
    let (dir, mut store) = temp_store();
    let orphan = store.paths().profiles_dir().join("Rabc.yaml");
    std::fs::write(&orphan, "KEEP ME\n").unwrap();

    let source = dir.path().join("foreign");
    foreign_installation(&source, "Rabc", "Rabc.yaml", "IMPORTED\n");
    let report = store.import_from(&source).unwrap();

    assert_eq!(
        report.renamed, 1,
        "the orphan must force a rename: {report:?}"
    );
    assert_eq!(std::fs::read_to_string(&orphan).unwrap(), "KEEP ME\n");
}

/// **FAILING:** the orphan check tests `profiles/<the source's file name>`,
/// but the copy goes to `profiles/<uid>.yaml`. When those two differ — and the
/// module's own comment says a hand-edited installation is exactly where they
/// differ — the orphan it was meant to protect is overwritten silently.
///
/// Minimal input: `profiles/Rabc.yaml` holds `KEEP ME`, the index knows no
/// `Rabc`, and the source index declares `uid: Rabc` with
/// `file: Rabc-source.yaml`.
#[test]
fn f8_an_orphan_document_is_still_overwritten_when_the_source_file_name_differs() {
    let (dir, mut store) = temp_store();
    let orphan = store.paths().profiles_dir().join("Rabc.yaml");
    std::fs::write(&orphan, "KEEP ME\n").unwrap();

    let source = dir.path().join("foreign");
    foreign_installation(&source, "Rabc", "Rabc-source.yaml", "IMPORTED\n");
    let report = store.import_from(&source).unwrap();

    assert_eq!(
        report.renamed, 1,
        "the collision check did not fire: {report:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&orphan).unwrap(),
        "KEEP ME\n",
        "a document no index entry owns was overwritten (report {report:?})"
    );
}

/// **FAILING — a new defect, not one of the eighteen:** nothing validates the
/// `uid` of an entry the index brings in, and the document's destination is
/// `profiles_dir/<uid>.yaml`, so a uid that climbs out of the directory writes
/// wherever it likes. Here the imported document lands on the *profile index
/// itself* and destroys it.
///
/// Minimal input: a source index holding
/// `uid: "../profiles"` and a document next to it.
#[test]
fn importing_a_uid_that_leaves_the_profiles_directory_overwrites_the_index() {
    let (dir, mut store) = temp_store();
    store.add(PrfItem::local("L1", "mine"));
    store.save().unwrap();
    let index_path = store.paths().profiles_index();
    let index_before = std::fs::read_to_string(&index_path).unwrap();
    assert!(index_before.contains("L1"), "{index_before}");

    let source = dir.path().join("foreign");
    std::fs::create_dir_all(source.join("profiles")).unwrap();
    std::fs::write(
        source.join("profiles.yaml"),
        "items:\n  - uid: \"../profiles\"\n    type: local\n    name: misplaced\n    \
         file: evil.yaml\n",
    )
    .unwrap();
    std::fs::write(source.join("profiles").join("evil.yaml"), "IMPORTED\n").unwrap();

    let report = store.import_from(&source).unwrap();
    assert_eq!(report.documents_copied, 1, "{report:?}");
    assert_eq!(
        std::fs::read_to_string(&index_path).unwrap(),
        index_before,
        "the import wrote outside the profiles directory, onto the index"
    );
}

// ============================================================== finding F9
// "the only non-idempotent case is a positional removal"

#[test]
fn f9_a_positional_removal_is_the_documented_exception() {
    let o = Overlay::from_yaml("remove: [\"rules[1]\"]\n").unwrap();
    let mut doc = json!({"rules": ["A,DIRECT", "B,DIRECT", "C,DIRECT"]});
    o.apply(&mut doc).unwrap();
    o.apply(&mut doc).unwrap();
    assert_eq!(
        doc["rules"],
        json!(["A,DIRECT"]),
        "the documented exception"
    );

    // Control: with unique names, a named removal really is idempotent.
    let named = Overlay::from_yaml("remove: [\"proxies[name=A]\"]\n").unwrap();
    let mut doc = json!({"proxies": [{"name": "A"}, {"name": "B"}]});
    named.apply(&mut doc).unwrap();
    let once = doc.clone();
    named.apply(&mut doc).unwrap();
    assert_eq!(doc, once);
}

/// Two more cases in which `Overlay::apply` was one-shot, neither of them a
/// positional removal — so the module doc's "every removal that names its
/// target can be applied any number of times" was not true.
///
/// The first is fixed: a named removal now removes *every* match, because
/// "remove the thing called A" is a statement about A and running it twice has
/// to mean what running it once meant.
///
/// The second cannot be fixed in the same way — a `set` addressed through a
/// selector renames the element it selected, so a second application has
/// nothing to find. What it must not do is *change the document* on the way to
/// failing, and the atomicity of `apply` is what guarantees that.
#[test]
fn f9_a_named_removal_is_idempotent_and_a_selector_rename_is_safe() {
    // (a) a named removal when the name is not unique. Duplicate names are the
    //     situation F5 was filed about: an imported profile re-adds a node.
    let named = Overlay::from_yaml("remove: [\"proxies[name=A]\"]\n").unwrap();
    let mut doc = json!({
        "proxies": [{"name": "A", "port": 1}, {"name": "A", "port": 2}, {"name": "B"}],
    });
    named.apply(&mut doc).unwrap();
    let once = doc.clone();
    named.apply(&mut doc).unwrap();
    assert_eq!(
        doc, once,
        "a removal that names its target removed a second element"
    );

    // (b) a set through a selector whose own field it rewrites. The second
    //     application has nothing to find, so it reports that — and leaves the
    //     document exactly as it was, which is the part that matters: half an
    //     override is worse than a refused one.
    let rename = Overlay::from_yaml("set: {\"proxies[name=A].name\": \"B\"}\n").unwrap();
    let mut doc = json!({"proxies": [{"name": "A"}]});
    rename.apply(&mut doc).unwrap();
    let once = doc.clone();
    let again = rename.apply(&mut doc);
    assert_eq!(doc, once, "a failed second apply must change nothing");
    assert!(
        again.is_err(),
        "this one cannot be a no-op: the selector has nothing left to match"
    );
}

// ============================================================= finding F10
// `Rule::parse` keeps what follows the policy of a payload-less rule, and the
// advisory about an ignored field is reachable. `FINAL` is gone: it is not a
// mihomo rule kind at all.

#[test]
fn f10_a_payload_less_rule_keeps_every_field_and_advises_about_it() {
    // Everything the parser accepts is re-emitted byte for byte.
    for src in [
        "MATCH,DIRECT",
        "MATCH,DIRECT,no-resolve",
        "MATCH,PROXY,no-resolve",
        "MATCH,漏网之鱼,no-resolve",
        "MATCH,DIRECT,no-resolve,no-resolve",
        "MATCH,DIRECT,1.2.3.4",
        "MATCH,DIRECT,no-resolve,1.2.3.4",
    ] {
        let r = Rule::parse(src).unwrap_or_else(|| panic!("{src} should parse"));
        assert_eq!(r.to_string(), src, "round trip failed for {src}");
    }

    // The fields after the policy are no longer dropped.
    let r = Rule::parse("MATCH,DIRECT,1.2.3.4,no-resolve").unwrap();
    assert_eq!(r.params, vec!["1.2.3.4", "no-resolve"]);
    assert!(r.is_terminal());
    assert!(r.no_resolve(), "the flag is still understood");

    // The advisory is reachable now — as a *warning*, because the core loads
    // this line and quietly ignores the field. Reporting an error would refuse
    // a configuration the core accepts, which is the one mistake a validator
    // must not make.
    let stray = config("mixed-port: 7890\nrules:\n  - MATCH,DIRECT,1.2.3.4\n");
    let advised = all_codes(&stray);
    assert!(
        advised.contains(&"W-MATCH-WITH-PAYLOAD"),
        "the advisory is still unreachable: {advised:?}"
    );
    assert!(
        error_codes(&stray).is_empty(),
        "the core loads this line: {:?}",
        error_codes(&stray)
    );
}

/// The parser used to accept a bare `MATCH` and invent `DIRECT`, so the round
/// trip was not byte-exact for an input it accepted. mihomo v1.19.31 requires
/// the policy field, and says so:
///
/// ```text
/// rules[0] [MATCH] error: format invalid
/// rules[0] [FINAL,DIRECT] error: format invalid
/// ```
///
/// Both are now refused by the parser, which is what makes the validator able
/// to report them (`E-RULE-MALFORMED`) instead of passing a line the core will
/// not load.
///
/// `FINAL` is not a mihomo rule kind at all, and it was in the payload-less
/// list. A rule type this build does not know is reported as `W-RULE-KIND` —
/// a warning rather than an error, because mihomo's rule table is closed today
/// but a core newer than this table might accept the type, and refusing a
/// configuration the core loads is the one mistake a validator must not make.
#[test]
fn f10_a_bare_match_is_refused_and_an_unknown_kind_is_reported() {
    for src in ["MATCH", "FINAL", "FINAL,PROXY"] {
        assert!(
            Rule::parse(src).is_none(),
            "{src} is not a rule the core can load"
        );
    }
    let bare = config("mixed-port: 7890\nrules:\n  - MATCH\n");
    assert!(
        error_codes(&bare).contains(&"E-RULE-MALFORMED"),
        "a line the core calls `format invalid` must be reported: {:?}",
        error_codes(&bare)
    );
    let unknown = config("mixed-port: 7890\nrules:\n  - FINAL,DIRECT,no-resolve\n");
    assert!(
        all_codes(&unknown).contains(&"W-RULE-KIND"),
        "`FINAL` is not a rule kind: {:?}",
        all_codes(&unknown)
    );
}

/// The flag list this once used was narrower than what the core ignores, so
/// it rejected `MATCH,DIRECT,src` — a line that loads and runs. mihomo
/// v1.19.31 `rules/common/base.go`: `case "MATCH": // MATCH doesn't contain
/// payload and params` takes `item[1]` as the target and discards the rest, and
/// `verge-mihomo -t` accepts the line. There is no list to keep in step any
/// more: *every* field after the policy is advised about, and none is an
/// error.
#[test]
fn f10_no_field_after_a_policy_is_rejected() {
    for src in [
        "MATCH,DIRECT,src",
        "MATCH,DIRECT,no-resolve",
        "MATCH,DIRECT,x,y,z",
    ] {
        let c = config(&format!("mixed-port: 7890\nrules:\n  - {src}\n"));
        assert!(
            error_codes(&c).is_empty(),
            "the core loads `{src}`: {:?}",
            error_codes(&c)
        );
    }
}

// ============================================================= finding F11
// `type: smart` is not a mihomo group type.

#[test]
fn f11_smart_is_reported_as_an_unknown_group_type() {
    let smart = config(
        "mixed-port: 7890\n\
         proxy-groups:\n  - { name: s, type: smart, proxies: [DIRECT] }\n\
         rules:\n  - MATCH,DIRECT\n",
    );
    let report = validate::check(&smart);
    let diag = report
        .diagnostics
        .iter()
        .find(|d| d.code == "E-GROUP-TYPE")
        .unwrap_or_else(|| panic!("`smart` was accepted: {}", report.render()));
    assert!(diag.message.contains("smart"), "{}", diag.message);
    let hint = diag.hint.clone().unwrap_or_default();
    assert!(
        !hint.contains("smart"),
        "the hint still recommends a type the core has no group for: {hint}"
    );
    assert!(!report.is_ok());

    // The types mihomo v1.19.31 does have are still accepted.
    for kind in ["select", "url-test", "fallback", "load-balance"] {
        let text = format!(
            "mixed-port: 7890\n\
             proxies:\n  - {{ name: p, type: socks5, server: 1.2.3.4, port: 1080 }}\n\
             proxy-groups:\n  - {{ name: g, type: {kind}, proxies: [p, DIRECT] }}\n\
             rules:\n  - MATCH,g\n"
        );
        let codes = error_codes(&config(&text));
        assert!(!codes.contains(&"E-GROUP-TYPE"), "type {kind}: {codes:?}");
        assert!(codes.is_empty(), "type {kind}: {codes:?}");
    }
}

/// **FAILING:** the fix removed `smart` and left `relay`, which mihomo
/// v1.19.31 also refuses. `adapter/outboundgroup/parser.go` at that tag:
///
/// ```text
/// case "relay":
///     return nil, fmt.Errorf("%w: The group [%s] with relay type was removed,
///     please using dialer-proxy instead", errType, groupName)
/// ```
///
/// — the config does not load, and `E-GROUP-TYPE`'s own hint still lists
/// `relay` among the types to use. The claim F11 was filed against is "no
/// false negatives: a config the core will reject must be an error".
#[test]
fn f11_relay_is_still_accepted_although_this_core_removed_it() {
    let relay = config(
        "mixed-port: 7890\n\
         proxies:\n  - { name: p, type: socks5, server: 1.2.3.4, port: 1080 }\n\
         proxy-groups:\n  - { name: r, type: relay, proxies: [p, DIRECT] }\n\
         rules:\n  - MATCH,r\n",
    );
    let codes = error_codes(&relay);
    assert!(
        codes.contains(&"E-GROUP-TYPE"),
        "`type: relay` is refused by mihomo v1.19.31, but validate is happy: {codes:?}"
    );
}

// ============================================================= finding F12
// `null` where a list is expected reads as the empty list; a numeric port is
// a port.

#[test]
fn f12_the_five_shapes_the_finding_names_now_parse() {
    let proxy: cvt_core::mihomo::types::ProxyView =
        serde_json::from_value(json!({"name": "x", "history": null})).unwrap();
    assert!(proxy.history.is_empty());

    let rules: RulesResponse = serde_json::from_value(json!({"rules": null})).unwrap();
    assert!(rules.rules.is_empty());

    let proxies: ProxiesResponse = serde_json::from_value(json!({"proxies": null})).unwrap();
    assert!(proxies.proxies.is_empty());

    let groups: GroupsResponse = serde_json::from_value(json!({"proxies": null})).unwrap();
    assert!(groups.proxies.is_empty());

    let conn: Connection = serde_json::from_value(json!({
        "id": "c1",
        "chains": null,
        "providerChains": [],
    }))
    .unwrap();
    assert!(conn.chains.is_empty());

    // A numeric port, a quoted port and an absent port all read the same.
    for (value, expected) in [
        (json!(50130), "50130"),
        (json!("50130"), "50130"),
        (Value::Null, ""),
    ] {
        let m: Metadata = serde_json::from_value(json!({"sourcePort": value})).unwrap();
        assert_eq!(m.source_port, expected, "sourcePort {value}");
        let m: Metadata = serde_json::from_value(json!({"destinationPort": value})).unwrap();
        assert_eq!(m.destination_port, expected, "destinationPort {value}");
    }
}

/// **FAILING:** the same shape still fails everywhere the author did not add
/// the attribute. `providerChains` is the sharpest of these: it sits next to
/// `chains` in the very same object, and `proxies` next to `rules` in the very
/// same API family.
#[test]
fn f12_the_same_shape_still_fails_for_the_list_fields_nobody_named() {
    let cases: Vec<(&str, Value)> = vec![
        (
            "Connection.providerChains",
            json!({"id": "c1", "chains": [], "providerChains": null}),
        ),
        (
            "ProxyProvidersResponse.providers",
            json!({"providers": null}),
        ),
        (
            "RuleProvidersResponse.providers",
            json!({"providers": null}),
        ),
        ("RuleProviderInfo.payload", json!({"payload": null})),
    ];
    let mut refused = Vec::new();
    for (name, value) in cases {
        let ok = match name {
            "Connection.providerChains" => {
                serde_json::from_value::<Connection>(value.clone()).is_ok()
            }
            "ProxyProvidersResponse.providers" => {
                serde_json::from_value::<ProxyProvidersResponse>(value.clone()).is_ok()
            }
            "RuleProvidersResponse.providers" => {
                serde_json::from_value::<RuleProvidersResponse>(value.clone()).is_ok()
            }
            _ => serde_json::from_value::<RuleProviderInfo>(value.clone()).is_ok(),
        };
        if !ok {
            refused.push(name);
        }
    }
    assert_eq!(
        refused,
        Vec::<&str>::new(),
        "these still refuse a `null` where the core may put one"
    );
}

// ============================================================= finding F13
// Appending a terminal rule replaces the catch-all; appending a non-terminal
// rule still inserts above it.

fn rules_of(doc: &Value) -> Vec<String> {
    doc["rules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap_or_default().to_owned())
        .collect()
}

fn terminal_rule(name: &str) -> bool {
    Rule::parse(name).is_some_and(|r| r.is_terminal())
}

#[test]
fn f13_the_two_single_item_cases_behave_as_the_claim_says() {
    let base = json!({"rules": ["DOMAIN,a.test,PROXY", "MATCH,DIRECT"]});

    // A non-terminal rule goes above the catch-all ...
    let o = Overlay::from_yaml("append:\n  rules: [\"DOMAIN,b.test,PROXY\"]\n").unwrap();
    let mut doc = base.clone();
    o.apply(&mut doc).unwrap();
    assert_eq!(
        rules_of(&doc),
        vec!["DOMAIN,a.test,PROXY", "DOMAIN,b.test,PROXY", "MATCH,DIRECT"]
    );

    // ... and a terminal one takes its place.
    let o = Overlay::from_yaml("append:\n  rules: [\"MATCH,REJECT\"]\n").unwrap();
    let mut doc = base;
    o.apply(&mut doc).unwrap();
    assert_eq!(rules_of(&doc), vec!["DOMAIN,a.test,PROXY", "MATCH,REJECT"]);
    assert_eq!(
        rules_of(&doc).iter().filter(|r| terminal_rule(r)).count(),
        1
    );
}

/// **FAILING:** a single `append` list holding a non-terminal rule *and* a
/// terminal one. The second iteration replaces the item the first iteration
/// had just inserted (the position of the catch-all is stale), so the appended
/// non-terminal rule is lost and the old catch-all stays — two terminal rules,
/// which `validate` reports as `E-UNREACHABLE-RULES`.
///
/// Minimal input:
/// ```yaml
/// append:
///   rules:
///     - DOMAIN-SUFFIX,b.test,PROXY
///     - MATCH,REJECT
/// ```
/// over `[DOMAIN-SUFFIX,a.test,PROXY, MATCH,DIRECT]`.
#[test]
fn f13_one_append_list_holding_a_rule_and_a_catch_all_keeps_at_most_one() {
    let o = Overlay::from_yaml(
        "append:\n  rules:\n    - DOMAIN-SUFFIX,b.test,PROXY\n    - MATCH,REJECT\n",
    )
    .unwrap();
    let mut doc = json!({"rules": ["DOMAIN-SUFFIX,a.test,PROXY", "MATCH,DIRECT"]});
    let log = o.apply(&mut doc).unwrap();
    let rules = rules_of(&doc);

    assert_eq!(
        rules,
        vec![
            "DOMAIN-SUFFIX,a.test,PROXY",
            "DOMAIN-SUFFIX,b.test,PROXY",
            "MATCH,REJECT"
        ],
        "the overlay reported {log:?}"
    );
    assert_eq!(
        rules.iter().filter(|r| terminal_rule(r)).count(),
        1,
        "two catch-alls: {rules:?}"
    );

    // Two catch-alls in a generated config are a blocking validation error.
    let mut text = String::from("mixed-port: 7890\nrules:\n");
    for rule in &rules {
        use std::fmt::Write as _;
        let _ = writeln!(text, "  - {rule}");
    }
    let codes = error_codes(&config(&text));
    assert!(
        !codes.contains(&"E-UNREACHABLE-RULES"),
        "the overlay produced a document validate rejects: {rules:?} {codes:?}"
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1500))]

    /// One appended rule at a time (the case the fix targets), generated over
    /// rules lists that mix terminal and non-terminal rules and names that
    /// already occur: the catch-all count may not grow, no supplied rule may
    /// land after the first catch-all, and a second apply changes nothing.
    #[test]
    fn f13_a_single_appended_rule_keeps_the_list_well_formed(
        base in prop::collection::vec(
            prop::sample::select(vec![
                "MATCH,DIRECT", "MATCH,REJECT", "FINAL,PROXY", "DOMAIN,a.test,PROXY",
                "DOMAIN-SUFFIX,b.test,DIRECT", "IP-CIDR,10.0.0.0/8,DIRECT",
            ]).prop_map(|s| Value::String(s.to_owned())),
            0..5,
        ),
        item in prop::sample::select(vec![
            "MATCH,DIRECT", "MATCH,REJECT", "FINAL,PROXY", "DOMAIN,c.test,PROXY",
        ]).prop_map(|s| Value::String(s.to_owned())),
    ) {
        let mut doc = json!({"rules": base});
        let original: Vec<Value> = doc["rules"].as_array().unwrap().clone();
        let overlay = Overlay::from_yaml("append:\n  rules: [\"MATCH,DIRECT\"]\n").unwrap();
        let overlay = Overlay {
            append: BTreeMap::from([("rules".to_owned(), vec![item.clone()])]),
            ..overlay
        };
        overlay.apply(&mut doc).unwrap();
        let after: Vec<Value> = doc["rules"].as_array().unwrap().clone();

        let count = |list: &[Value]| {
            list.iter()
                .filter(|v| v.as_str().is_some_and(terminal_rule))
                .count()
        };
        let before_terminals = count(&original);
        let after_terminals = count(&after);
        let supplied = item.as_str().unwrap();
        let is_new = !original.iter().any(|v| v.as_str() == Some(supplied))
            && !supplied.trim().is_empty();

        // A list that already had a catch-all may not end up with more of them:
        // that is the defect F13 is about. A list that had none may gain one —
        // appending a catch-all to a list without one is a legitimate overlay.
        if before_terminals >= 1 {
            prop_assert!(
                after_terminals <= before_terminals,
                "appending made the catch-all count grow: before={:?} after={:?}",
                original,
                after
            );
        }

        if is_new && terminal_rule(supplied) {
            // The appended catch-all must be there, and when it replaced the
            // only existing one there must be exactly one left.
            prop_assert!(
                after.iter().any(|v| v.as_str() == Some(supplied)),
                "the appended catch-all is missing: {:?} -> {:?}",
                original,
                after
            );
            if before_terminals == 1 {
                prop_assert_eq!(
                    after_terminals,
                    1,
                    "the old catch-all was left behind: {:?}",
                    after
                );
            }
        } else if is_new {
            let at = after
                .iter()
                .position(|v| v.as_str() == Some(supplied))
                .expect("the appended rule is missing");
            let first_terminal = after
                .iter()
                .position(|v| v.as_str().is_some_and(terminal_rule))
                .unwrap_or(after.len());
            prop_assert!(
                at < first_terminal,
                "the appended rule landed after the catch-all: {:?}",
                after
            );
        }

        let mut twice = doc.clone();
        overlay.apply(&mut twice).unwrap();
        prop_assert_eq!(twice, doc, "applying the same append again changed it");
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(600))]

    /// The same claim over append lists of *two* items, which is where a
    /// stale catch-all position used to be visible: an append whose second
    /// item overwrote the first, leaving the log saying it had added both.
    ///
    /// Either every item the patch names is in the result, or the patch is
    /// refused — which is what happens when it names two catch-alls, since
    /// only one of them could ever run. Silently naming fewer rules than the
    /// document did is the one outcome that is not allowed.
    #[test]
    fn f13_an_append_of_two_items_either_lands_whole_or_is_refused(
        base in prop::collection::vec(
            prop::sample::select(vec![
                "MATCH,DIRECT", "DOMAIN,a.test,PROXY", "DOMAIN-SUFFIX,b.test,DIRECT",
            ]).prop_map(|s| Value::String(s.to_owned())),
            0..4,
        ),
        first in prop::sample::select(vec![
            "MATCH,DIRECT", "MATCH,REJECT", "DOMAIN,c.test,PROXY",
        ]).prop_map(|s| Value::String(s.to_owned())),
        second in prop::sample::select(vec![
            "MATCH,DIRECT", "MATCH,REJECT", "DOMAIN,d.test,PROXY",
        ]).prop_map(|s| Value::String(s.to_owned())),
    ) {
        let items = vec![first, second];
        let mut doc = json!({"rules": base});
        let original: Vec<Value> = doc["rules"].as_array().unwrap().clone();
        let overlay = Overlay {
            append: BTreeMap::from([("rules".to_owned(), items.clone())]),
            ..Overlay::default()
        };
        let Ok(log) = overlay.apply(&mut doc) else {
            // Refused, which is the honest answer for a patch naming two
            // catch-alls — and it must leave the document exactly as it was.
            prop_assert_eq!(
                doc["rules"].as_array().unwrap().clone(),
                original,
                "a refused append must not touch the rules it refused"
            );
            return Ok(());
        };
        let after: Vec<Value> = doc["rules"].as_array().unwrap().clone();

        for item in &items {
            let text = item.as_str().unwrap();
            let already = original.iter().any(|v| v.as_str() == Some(text));
            prop_assert!(
                already || after.contains(item),
                "`{text}` was named in the append list ({log:?}) but is not in {:?}",
                after
            );
        }
        let terminals = |list: &[Value]| {
            list.iter()
                .filter(|v| v.as_str().is_some_and(terminal_rule))
                .count()
        };
        if terminals(&original) >= 1 {
            prop_assert!(
                terminals(&after) <= terminals(&original),
                "the catch-all count grew: {:?} -> {:?}",
                original,
                after
            );
        }
    }
}

// ============================================================= finding F14
// A pure reorder of a named list produces `Change::Reordered`.

proptest! {
    #![proptest_config(ProptestConfig::with_cases(3000))]

    /// Generated named lists: two documents differ if and only if the diff is
    /// non-empty, and when the *only* difference is the order of the shared
    /// names there is a `Reordered` entry under the right top-level key.
    #[test]
    fn f14_a_named_list_that_only_reordered_is_reported(
        names in prop::collection::vec("[A-C]", 0..5),
        values in prop::collection::vec(0u8..3, 0..5),
        order in prop::collection::vec(0u8..7, 0..5),
    ) {
        let n = names.len().min(values.len()).min(order.len());
        let items: Vec<Value> = (0..n)
            .map(|i| json!({"name": names[i], "v": values[i]}))
            .collect();
        let mut shuffled: Vec<(u8, &Value)> =
            order[..n].iter().copied().zip(items.iter()).collect();
        shuffled.sort_by_key(|(k, _)| *k);
        let after: Vec<Value> = shuffled.into_iter().map(|(_, v)| v.clone()).collect();

        // The named-list shape is the one under test: every element must have
        // a string name and no name may repeat, or the diff legitimately falls
        // back to reporting the whole list.
        let unique = {
            let mut seen = std::collections::BTreeSet::new();
            names[..n].iter().all(|name| seen.insert(name.clone()))
        };
        prop_assume!(unique);

        let before = json!({"proxy-groups": items});
        let after = json!({"proxy-groups": after});
        let d = diff(&before, &after);

        if before == after {
            prop_assert!(d.is_empty(), "{:?}", d.entries);
        } else {
            prop_assert!(
                !d.is_empty(),
                "two different documents produced no diff at all: {before} vs {after}"
            );
            let same_set = items.len() == after["proxy-groups"].as_array().unwrap().len();
            if same_set {
                let reordered = d.entries.iter().any(|e| {
                    e.change == Change::Reordered
                        && e.root.as_deref() == Some("proxy-groups")
                        && e.path == "proxy-groups"
                });
                prop_assert!(
                    reordered,
                    "a pure reorder was reported as something else: {:?}",
                    d.entries
                );
            }
            prop_assert_eq!(d.touched_keys(), vec!["proxy-groups".to_owned()]);
            prop_assert_eq!(d.for_top_level("proxy-groups").len(), d.len());
        }
    }

    /// The nested form: the same list one level down, under a strange key.
    #[test]
    fn f14_a_nested_reorder_is_reported_under_its_root(
        first in "[a-c]{1,4}",
        second in "[a-c]{1,4}",
    ) {
        prop_assume!(first != second);
        let before = json!({"my.key": {"groups": [
            {"name": first, "v": 1}, {"name": second, "v": 2}
        ]}});
        let after = json!({"my.key": {"groups": [
            {"name": second, "v": 2}, {"name": first, "v": 1}
        ]}});
        let d = diff(&before, &after);
        prop_assert!(!d.is_empty());
        prop_assert_eq!(d.touched_keys(), vec!["my.key".to_owned()]);
        prop_assert_eq!(
            d.for_top_level("my.key").len(),
            d.len(),
            "entries are not attributed to the key they live under"
        );
        prop_assert!(
            d.entries.iter().any(|e| {
                e.change == Change::Reordered && e.path == "my.key.groups"
            }),
            "a pure reorder was not reported as one: {:?}",
            d.entries
        );
    }
}

// ============================================================= finding F15
// A `DiffEntry` carries its top-level key rather than recovering it from the
// path string.

#[test]
fn f15_strange_top_level_keys_are_carried_whole() {
    /// The pre-fix recovery: the top-level key was read back off the path
    /// string, which truncates at the first `.` or `[`. Kept here so this test
    /// can be *seen* to be capable of failing on the code it replaces.
    fn split_based_root(path: &str) -> String {
        path.split(['.', '[']).next().unwrap_or_default().to_owned()
    }
    for key in ["my.key", "a[b]", "x[name=Y]"] {
        assert_ne!(
            split_based_root(key),
            key,
            "`{key}` would not have caught the old recovery"
        );
    }

    for key in [
        "",
        "my.key",
        "a[b]",
        "x[name=Y]",
        "proxy-groups[name=P].url",
        ".",
        "[",
        "]",
        "name=",
        "ends.",
    ] {
        let mut before = Map::new();
        let mut after = Map::new();
        before.insert(key.to_owned(), json!({"v": 1}));
        after.insert(key.to_owned(), json!({"v": 2}));
        let d = diff(&Value::Object(before), &Value::Object(after));
        assert_eq!(
            d.touched_keys(),
            vec![key.to_owned()],
            "top-level key {key:?} was not carried whole"
        );
        assert_eq!(
            d.for_top_level(key).len(),
            1,
            "for_top_level({key:?}) found nothing"
        );
        assert_eq!(d.entries[0].root.as_deref(), Some(key));
    }

    // A nested difference must keep the *parent's* key, however strange it is.
    let d = diff(
        &json!({"my.key": {"deep": {"v": 1}}}),
        &json!({"my.key": {"deep": {"v": 2}}}),
    );
    assert_eq!(d.touched_keys(), vec!["my.key"]);
    assert!(d.for_top_level("my").is_empty(), "the path was split again");

    // ... and a genuinely nested key is not mistaken for a dotted top-level one.
    let d = diff(&json!({"my": {"key": 1}}), &json!({"my": {"key": 2}}));
    assert_eq!(d.touched_keys(), vec!["my"]);
    assert!(d.for_top_level("my.key").is_empty());
    assert_eq!(d.for_top_level("my").len(), 1);

    // Two keys where one is a prefix of the other: attribution must be exact,
    // not a prefix match.
    let mut before = Map::new();
    let mut after = Map::new();
    for key in ["my", "my.key"] {
        before.insert(key.to_owned(), json!(1));
        after.insert(key.to_owned(), json!(2));
    }
    let d = diff(&Value::Object(before), &Value::Object(after));
    assert_eq!(d.touched_keys(), vec!["my".to_owned(), "my.key".to_owned()]);
    assert_eq!(d.for_top_level("my").len(), 1);
    assert_eq!(d.for_top_level("my.key").len(), 1);

    // An empty top-level key is not the same as "no key at all".
    let d = diff(&json!({"": 1}), &json!({"": 2}));
    assert_eq!(d.touched_keys(), vec![String::new()]);
    let d = diff(&json!([1]), &json!([2]));
    assert!(d.touched_keys().is_empty(), "a list document has no keys");
    assert_eq!(d.entries[0].root, None);
}

// ============================================================= finding F16
// The sequence-patch note prints `(before -> after)`.

struct PipelineFixture {
    _dir: TempDir,
    store: ProfileStore,
    pipeline: Pipeline,
}

impl PipelineFixture {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let paths = AppPaths::new(dir.path());
        paths.ensure_dirs().unwrap();
        let store = ProfileStore::load(&paths).unwrap();
        let pipeline = Pipeline::new(paths);
        Self {
            _dir: dir,
            store,
            pipeline,
        }
    }

    fn add(&mut self, item: PrfItem, body: &str) -> String {
        let uid = self.store.add(item);
        let stored = self.store.get(&uid).unwrap().clone();
        self.store.write_document(&stored, body).unwrap();
        uid
    }

    fn base(&mut self) {
        let uid = self.add(PrfItem::local("L1", "base"), BASE);
        self.store.set_current(&uid).unwrap();
    }

    fn note_for(&self, uid: &str) -> String {
        let outcome = self.pipeline.generate(&self.store).unwrap();
        outcome
            .applied
            .iter()
            .find(|a| a.uid == uid)
            .unwrap_or_else(|| panic!("no entry for {uid}: {:?}", outcome.applied))
            .note
            .clone()
    }
}

#[test]
fn f16_the_note_reads_before_then_after() {
    let mut grow = PipelineFixture::new();
    grow.base();
    let uid = grow.add(
        PrfItem::patch("r1", "rules", ProfileType::Rules),
        "prepend:\n  - DOMAIN-SUFFIX,intranet.test,DIRECT\nappend:\n  - DOMAIN-SUFFIX,corp.test,DIRECT\n",
    );
    // BASE has two rules; the patch adds two, so the note must grow left to right.
    let note = grow.note_for(&uid);
    assert!(
        note.contains("(2 -> 4)"),
        "a growing list must read (2 -> 4), got {note:?}"
    );
    assert!(!note.contains("(4 -> 2)"), "{note:?}");

    let mut shrink = PipelineFixture::new();
    shrink.base();
    let uid = shrink.add(
        PrfItem::patch("r2", "rules", ProfileType::Rules),
        "delete:\n  - DOMAIN-SUFFIX,google.com,PROXY\n",
    );
    let note = shrink.note_for(&uid);
    assert!(
        note.contains("(2 -> 1)"),
        "a shrinking list must read (2 -> 1), got {note:?}"
    );
    assert!(!note.contains("(1 -> 2)"), "{note:?}");
}

// ============================================================= finding F17
// `Overlay::default()` and `Overlay::from_yaml("{}")` are the same overlay.

#[test]
fn f17_the_two_spellings_of_an_empty_overlay_agree() {
    let from_default = Overlay::default();
    let from_document = Overlay::from_yaml("{}").expect("{} is an empty overlay");
    assert_eq!(from_default, from_document);
    assert!(from_default.append_before_terminal);
    assert!(from_document.append_before_terminal);
    assert_eq!(
        Overlay::from_yaml("append_before_terminal: true\n").unwrap(),
        from_default
    );

    // Both spellings do nothing, the same way, and both round trip.
    let doc = json!({"rules": ["DOMAIN,a.test,PROXY", "MATCH,DIRECT"]});
    let mut a = doc.clone();
    let mut b = doc.clone();
    assert!(from_default.apply(&mut a).unwrap().is_empty());
    assert!(from_document.apply(&mut b).unwrap().is_empty());
    assert_eq!(a, b);
    assert_eq!(a, doc);
    assert_eq!(
        Overlay::from_yaml(&from_default.to_yaml().unwrap()).unwrap(),
        from_default,
        "the default overlay must survive its own YAML"
    );

    // The Rust default behaves like the documented default: an appended rule
    // must not land after the catch-all.
    let built = Overlay {
        append: BTreeMap::from([(
            "rules".to_owned(),
            vec![json!("DOMAIN-SUFFIX,x.test,DIRECT")],
        )]),
        ..Overlay::default()
    };
    let mut c = doc;
    built.apply(&mut c).unwrap();
    assert_eq!(
        rules_of(&c),
        vec![
            "DOMAIN,a.test,PROXY",
            "DOMAIN-SUFFIX,x.test,DIRECT",
            "MATCH,DIRECT"
        ]
    );
}

// ============================================================= finding F18
// `probe()` must not run the route it probes.

type Seen = Arc<Mutex<Vec<(String, String, String)>>>;

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// A hand-rolled HTTP/1.1 responder on a real `TcpListener`: it records the
/// method, target and body of every request and answers the way a real core
/// does for the five probes. One request per connection.
async fn fake_core() -> (Endpoint, Seen) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let mut buf: Vec<u8> = Vec::new();
            let mut chunk = [0u8; 2048];
            let mut header_end: Option<usize> = None;
            let mut content_length = 0usize;
            loop {
                if header_end.is_none()
                    && let Some(pos) = find(&buf, b"\r\n\r\n")
                {
                    header_end = Some(pos);
                    let head = String::from_utf8_lossy(&buf[..pos]).into_owned();
                    content_length = head
                        .lines()
                        .find_map(|line| {
                            let (k, v) = line.split_once(':')?;
                            k.eq_ignore_ascii_case("content-length")
                                .then(|| v.trim().parse::<usize>().ok())?
                        })
                        .unwrap_or(0);
                }
                if let Some(pos) = header_end
                    && buf.len() >= pos + 4 + content_length
                {
                    break;
                }
                match socket.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
            }
            let head = header_end.map(|pos| String::from_utf8_lossy(&buf[..pos]));
            let first = head
                .as_deref()
                .and_then(|h| h.lines().next())
                .unwrap_or_default()
                .to_owned();
            let mut parts = first.split_whitespace();
            let method = parts.next().unwrap_or_default().to_owned();
            let target = parts.next().unwrap_or_default().to_owned();
            let body = header_end.map_or_else(String::new, |pos| {
                String::from_utf8_lossy(&buf[pos + 4..]).into_owned()
            });

            // The surface the report describes: a live v1.19.31 core with no
            // debug router (plain-text 404) and every other optional route
            // mounted (405 to the wrong method).
            let (status, ctype, out) = match (method.as_str(), target.as_str()) {
                ("GET", "/version") => (
                    "200 OK",
                    "application/json",
                    r#"{"meta":true,"version":"v1.19.31"}"#.to_owned(),
                ),
                ("PUT", "/debug/gc") => (
                    "404 Not Found",
                    "text/plain; charset=utf-8",
                    "404 page not found\n".to_owned(),
                ),
                _ => (
                    "405 Method Not Allowed",
                    "text/plain; charset=utf-8",
                    "Method Not Allowed\n".to_owned(),
                ),
            };
            let response = format!(
                "HTTP/1.1 {status}\r\ncontent-type: {ctype}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{out}",
                out.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.shutdown().await;
            sink.lock().unwrap().push((method, target, body));
        }
    });
    (Endpoint::tcp(format!("127.0.0.1:{port}"), None), seen)
}

#[tokio::test]
async fn f18_probing_uses_a_method_the_route_is_not_registered_for() {
    let (endpoint, seen) = fake_core().await;
    let client = Client::new(endpoint).unwrap();

    let caps = client.probe().await.unwrap();
    assert_eq!(caps.version, "1.19.31");
    assert!(caps.rules_disable && caps.configs_write && caps.upgrade);
    assert!(
        !caps.debug,
        "a plain-text 404 means the debug subtree is absent"
    );

    let probes: Vec<(String, String)> = seen
        .lock()
        .unwrap()
        .iter()
        .map(|(m, t, _)| (m.clone(), t.clone()))
        .collect();
    assert_eq!(
        probes,
        vec![
            ("GET".to_owned(), "/version".to_owned()),
            ("PATCH".to_owned(), "/rules/disable".to_owned()),
            ("PATCH".to_owned(), "/configs".to_owned()),
            ("PUT".to_owned(), "/debug/gc".to_owned()),
            ("PUT".to_owned(), "/upgrade/geo".to_owned()),
        ],
        "probing must ask about /upgrade/geo with the wrong method"
    );
    for (method, target, body) in seen.lock().unwrap().iter() {
        assert_ne!(
            (method.as_str(), target.as_str()),
            ("POST", "/upgrade/geo"),
            "probing ran the geodata updater"
        );
        assert!(body.is_empty(), "probing sent a body to {target}");
    }

    // The assertion above is not vacuous: `upgrade_geo` is exactly the request
    // the pre-fix probe used to send, and the recorder sees it.
    let before = seen.lock().unwrap().len();
    let _ = client.upgrade_geo().await;
    let (method, target) = {
        let after = seen.lock().unwrap();
        assert_eq!(after.len(), before + 1);
        (after[before].0.clone(), after[before].1.clone())
    };
    assert_eq!(
        (method.as_str(), target.as_str()),
        ("POST", "/upgrade/geo"),
        "the recorder cannot see the call the old probe made, so it proves nothing"
    );
}

// ============================================================= finding F19
// A failed reload reports the real cause, not the rollback bookkeeping.

fn service_fixture() -> (TempDir, Service) {
    let dir = TempDir::new().unwrap();
    let service = Service::open(AppPaths::new(dir.path())).unwrap();
    (dir, service)
}

fn seed_base(service: &Service) {
    let mut store = service.store().unwrap();
    let uid = store.add(PrfItem::local("L1", "base"));
    let item = store.get(&uid).unwrap().clone();
    store.write_document(&item, BASE).unwrap();
    store.set_current(&uid).unwrap();
    store.save().unwrap();
}

/// A binary that exists but refuses every document — the case rollback exists
/// for. It is `/bin/false`, never a mihomo build, so no core is started.
fn refuse_everything(service: &mut Service, rollback: bool) {
    let mut settings = service.settings().clone();
    settings.core.binary = Some(PathBuf::from("/bin/false"));
    settings.core.rollback_on_failure = rollback;
    service.set_settings(settings);
}

fn is_rollback_bookkeeping(err: &Error) -> bool {
    matches!(
        err,
        Error::InvalidValue {
            field: "rollback",
            ..
        }
    ) || err.to_string().contains("snapshot")
}

/// The two ways the snapshot list can be empty — nothing was ever committed,
/// and a first apply that had nothing before it — must report the same thing:
/// the cause.
#[tokio::test]
async fn f19_both_spellings_of_no_snapshot_report_the_real_cause() {
    // (a) nothing has been generated yet: the snapshot directory does not exist.
    let (_d, mut service) = service_fixture();
    seed_base(&service);
    refuse_everything(&mut service, true);
    assert!(service.pipeline().snapshots().unwrap().is_empty());
    let err = service.reload(ReloadMode::Auto).await.unwrap_err();
    assert!(
        !is_rollback_bookkeeping(&err),
        "bookkeeping displaced the cause: {err:?}"
    );
    assert!(
        err.to_string().contains("configuration"),
        "the cause is that nothing has been generated: {err:?}"
    );

    // (b) a first apply was committed, so the directory exists but is empty.
    let (_d, mut service) = service_fixture();
    seed_base(&service);
    let first = service.generate().unwrap();
    service.pipeline().commit(&first, false).unwrap();
    assert!(
        service.pipeline().snapshots().unwrap().is_empty(),
        "a first commit has nothing to snapshot"
    );
    refuse_everything(&mut service, true);
    let err = service.reload(ReloadMode::Auto).await.unwrap_err();
    assert!(
        !is_rollback_bookkeeping(&err),
        "bookkeeping displaced the cause: {err:?}"
    );
    assert!(matches!(err, Error::ProcessFailed { .. }), "{err:?}");

    // (c) rollback switched off is the other spelling of "do not undo"; the
    //     cause must survive that too, snapshot or no snapshot.
    let (_d, mut service) = service_fixture();
    seed_base(&service);
    let first = service.generate().unwrap();
    service.pipeline().commit(&first, false).unwrap();
    refuse_everything(&mut service, false);
    let err = service.reload(ReloadMode::Auto).await.unwrap_err();
    assert!(
        !is_rollback_bookkeeping(&err),
        "bookkeeping displaced the cause: {err:?}"
    );
    assert!(matches!(err, Error::ProcessFailed { .. }), "{err:?}");
}

/// The complement, so the three cases above cannot pass by disabling rollback:
/// with a previous document on disk, a rejected one is still undone.
#[tokio::test]
async fn f19_a_rejected_document_is_still_rolled_back_when_there_is_one() {
    let (_d, mut service) = service_fixture();
    seed_base(&service);
    let first = service.generate().unwrap();
    service.pipeline().commit(&first, false).unwrap();

    {
        let store = service.store().unwrap();
        let item = store.get("L1").unwrap().clone();
        store
            .write_document(&item, &BASE.replace("7890", "7891"))
            .unwrap();
        store.save().unwrap();
    }
    let second = service.generate().unwrap();
    service.pipeline().commit(&second, false).unwrap();
    assert_ne!(first.yaml, second.yaml);
    assert!(!service.pipeline().snapshots().unwrap().is_empty());

    refuse_everything(&mut service, true);
    let _ = service.reload(ReloadMode::Auto).await;

    let restored = std::fs::read_to_string(service.paths().runtime_config()).unwrap();
    assert_eq!(
        restored, first.yaml,
        "the working document must be restored"
    );
}

// ================================================ the self-found contradiction
// "an overlay that gives a path a list while setting a key *inside* that path
// is now refused rather than applying once and failing the second time"

/// **FAILING:** the refusal is too broad. It fires whenever a `set` path
/// merely *starts with* a list path followed by `[`, but a `set` of a list
/// *element* needs that path to be a list, not a mapping — so this overlay
/// works (and is idempotent) while `from_yaml` rejects it.
///
/// Minimal input:
/// ```yaml
/// set:
///   "dns.nameserver[0]": "9.9.9.9"
/// append:
///   dns.nameserver: ["8.8.8.8"]
/// ```
#[test]
fn the_self_contradiction_check_refuses_a_working_overlay() {
    // Each half on its own, applied in the order `apply` uses, is legal and
    // idempotent — which is what the combined document asks for.
    let set_only = Overlay::from_yaml("set:\n  \"dns.nameserver[0]\": \"9.9.9.9\"\n").unwrap();
    let append_only = Overlay::from_yaml("append:\n  dns.nameserver: [\"8.8.8.8\"]\n").unwrap();
    let mut doc = json!({"dns": {"nameserver": ["1.1.1.1"]}});
    set_only.apply(&mut doc).unwrap();
    append_only.apply(&mut doc).unwrap();
    assert_eq!(doc, json!({"dns": {"nameserver": ["9.9.9.9", "8.8.8.8"]}}));
    set_only.apply(&mut doc).unwrap();
    append_only.apply(&mut doc).unwrap();
    assert_eq!(
        doc,
        json!({"dns": {"nameserver": ["9.9.9.9", "8.8.8.8"]}}),
        "the two edits are idempotent together"
    );

    // The same document is refused before it starts.
    let combined = Overlay::from_yaml(
        "set:\n  \"dns.nameserver[0]\": \"9.9.9.9\"\nappend:\n  dns.nameserver: [\"8.8.8.8\"]\n",
    );
    assert!(
        combined.is_ok(),
        "a working overlay was refused: {}",
        combined.err().map(|e| e.to_string()).unwrap_or_default()
    );
}

/// **FAILING:** the contradiction in the other direction is not refused at all,
/// so it *is* applied half-way: `set` writes, then the list edit fails and the
/// document keeps the first edit even though `apply` returned `Err`. The
/// module's promise for a refused overlay is that it does not touch the
/// document.
///
/// Minimal input:
/// ```yaml
/// set:
///   "a": [1, 2]
/// append:
///   "a.b": ["x"]
/// ```
#[test]
fn the_reverse_contradiction_is_applied_half_way() {
    let o = Overlay::from_yaml("set:\n  \"a\": [1, 2]\nappend:\n  \"a.b\": [\"x\"]\n");
    let Ok(o) = o else {
        // If the author ever refuses this shape too, the test is satisfied.
        return;
    };
    let mut doc = json!({});
    let result = o.apply(&mut doc).map(|log| log.len());
    assert_eq!(
        doc,
        json!({}),
        "the overlay is not refused up front, so a failed apply must not leave \
         half of its edits in place; `apply` returned {result:?} and left {doc}"
    );
}
