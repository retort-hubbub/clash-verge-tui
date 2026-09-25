//! A **third**, independent pass over the newest round of fixes in `cvt-core`.
//!
//! Written against the author's claims, not their tests. Three things are
//! deliberately different from `recheck.rs`:
//!
//! * every claim that is behavioural over a space of inputs is attacked with a
//!   generated case rather than the input the author happened to pick;
//! * every claim about what mihomo accepts was settled by running the real core
//!   (`/usr/bin/verge-mihomo`, `v1.19.31`) with `-t`, and the exact command and
//!   output are quoted in the doc comment above the test;
//! * tests named `defect_*` assert the behaviour the *claim* promises and are
//!   expected to fail. A failing `defect_` test is a finding, not a broken test.
//!
//! Nothing here starts or stops a long-running core: `-t` validates and exits.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::too_many_lines
)]

use std::collections::BTreeMap;
use std::path::{Path as StdPath, PathBuf};

use cvt_core::enhance::overlay::Overlay;
use cvt_core::enhance::path::{self, Path};
use cvt_core::enhance::pipeline::Pipeline;
use cvt_core::mihomo::types::{
    Connection, GroupsResponse, Metadata, ProxiesResponse, ProxyProviderInfo,
    ProxyProvidersResponse, ProxyView, RuleInfo, RuleProviderInfo, RuleProvidersResponse,
    RulesResponse,
};
use cvt_core::model::config::Config;
use cvt_core::model::rule::Rule;
use cvt_core::profile::item::{PrfItem, ProfileType};
use cvt_core::profile::store::{Index, ProfileStore, document_path};
use cvt_core::{AppPaths, validate};
use proptest::prelude::*;
use serde_json::{Value, json};
use tempfile::TempDir;

// ------------------------------------------------------------------ helpers

/// A document that passes `validate` and declares the control plane.
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

fn error_codes(c: &Config) -> Vec<&'static str> {
    validate::check(c).errors_iter().map(|d| d.code).collect()
}

fn all_codes(c: &Config) -> Vec<&'static str> {
    validate::check(c)
        .diagnostics
        .iter()
        .map(|d| d.code)
        .collect()
}

fn rules_of(doc: &Value) -> Vec<String> {
    doc["rules"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|v| v.as_str().unwrap_or_default().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

fn is_terminal(text: &str) -> bool {
    Rule::parse(text).is_some_and(|r| r.is_terminal())
}

fn temp_store() -> (TempDir, ProfileStore) {
    let dir = TempDir::new().unwrap();
    let paths = AppPaths::new(dir.path());
    paths.ensure_dirs().unwrap();
    let store = ProfileStore::load(&paths).unwrap();
    (dir, store)
}

/// An overlay built in code: `validate` is the same code path `from_yaml` takes,
/// without YAML quoting getting in the way of a generated path.
fn overlay(set: &[(&str, Value)], append: &[(&str, Vec<Value>)]) -> Overlay {
    Overlay {
        set: set
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect(),
        append: append
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect(),
        ..Overlay::default()
    }
}

/// Every file under `root`, as `relative path -> contents`.
fn snapshot(root: &StdPath) -> BTreeMap<String, String> {
    fn walk(dir: &StdPath, root: &StdPath, out: &mut BTreeMap<String, String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            if path.is_dir() {
                out.insert(format!("{rel}/"), String::new());
                walk(&path, root, out);
            } else {
                let body =
                    std::fs::read_to_string(&path).unwrap_or_else(|_| "<not utf-8>".to_owned());
                out.insert(rel, body);
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// Write a `clash-verge-rev`-shaped source directory from an index document.
fn foreign_installation(root: &StdPath, index: &Index, documents: &[(&str, &str)]) {
    std::fs::create_dir_all(root.join("profiles")).unwrap();
    std::fs::write(
        root.join("profiles.yaml"),
        serde_norway::to_string(index).unwrap(),
    )
    .unwrap();
    for (name, body) in documents {
        std::fs::write(root.join("profiles").join(name), body).unwrap();
    }
}

/// A proptest configuration that never writes a regression file: this suite is
/// one file, and a failing case has to stay reproducible from the test alone.
fn cases(n: u32) -> ProptestConfig {
    ProptestConfig {
        cases: n,
        failure_persistence: None,
        ..ProptestConfig::default()
    }
}

// ==========================================================================
// 1. The inverted assertions in `recheck.rs`
// ==========================================================================

/// CLAIM: "policy names are matched **exactly** (so `MATCH,direct` reports
/// `E-DANGLING-POLICY`)".
///
/// `recheck.rs::f1_a_case_mismatched_built_in_policy_is_reported` is an
/// *inverted* assertion: the second reviewer recorded that the validator
/// accepted `MATCH,direct`, and the author now asserts it is reported.
///
/// VERDICT: the inversion is RIGHT. Live core, `v1.19.31`, one config per row:
///
/// ```text
/// $ verge-mihomo -t -d <dir> -f <file>      # rules: [MATCH,<spelling>]
/// MATCH,DIRECT       -> test is successful
/// MATCH,direct       -> test failed   rules[0] [MATCH,direct] error: proxy [direct] not found
/// MATCH,Direct       -> test failed
/// MATCH,GLOBAL       -> test is successful
/// MATCH,global       -> test failed
/// MATCH,PASS-RULE    -> test is successful
/// MATCH,pass-rule    -> test failed
/// MATCH,COMPATIBLE   -> test is successful
/// MATCH,compatible   -> test failed
/// MATCH,REJECT-DROP  -> test is successful
/// MATCH,reject-drop  -> test failed
/// MATCH,PASS         -> test is successful
/// MATCH,pass         -> test failed
/// ```
///
/// What this test does that the author's does not: it drives the whole table
/// through `validate::check` and asserts the validator and the core *agree on
/// every row*, in both directions. The author's test checks one spelling that
/// must be refused and one that must be accepted.
#[test]
fn inverted_1_the_policy_table_matches_the_core_exactly() {
    // (spelling, the core loaded it)
    let table: &[(&str, bool)] = &[
        ("DIRECT", true),
        ("direct", false),
        ("Direct", false),
        ("GLOBAL", true),
        ("global", false),
        ("PASS-RULE", true),
        ("pass-rule", false),
        ("COMPATIBLE", true),
        ("compatible", false),
        ("REJECT-DROP", true),
        ("reject-drop", false),
        ("PASS", true),
        ("pass", false),
    ];
    for (spelling, core_accepts) in table {
        let c = config(&format!("mixed-port: 7890\nrules:\n  - MATCH,{spelling}\n"));
        let reported = error_codes(&c).contains(&"E-DANGLING-POLICY");
        assert_eq!(
            reported,
            !core_accepts,
            "`MATCH,{spelling}`: the core {} it, validate {} it",
            if *core_accepts { "loads" } else { "refuses" },
            if reported { "refuses" } else { "loads" }
        );
    }

    // A name that exists in the document is still matched exactly, so a group
    // called `proxy` is not the group called `PROXY`.
    let named = config(
        "mixed-port: 7890\n\
         proxy-groups:\n  - { name: proxy, type: select, proxies: [DIRECT] }\n\
         rules:\n  - MATCH,PROXY\n  - MATCH,proxy\n",
    );
    let codes = validate::check(&named)
        .diagnostics
        .iter()
        .map(|d| (d.code, d.location.clone()))
        .collect::<Vec<_>>();
    assert!(
        codes
            .iter()
            .any(|(c, at)| *c == "E-DANGLING-POLICY" && at.as_deref() == Some("rules[0]")),
        "`MATCH,PROXY` points at a group that does not exist: {codes:?}"
    );
}

/// CLAIM: the orphan-document check "tests the path about to be written",
/// looping "until both the uid and that destination are free".
///
/// `recheck.rs::f8_an_orphan_document_is_still_overwritten_when_the_source_file_name_differs`
/// is an inverted assertion (the defect it recorded is gone; its doc comment
/// still says `**FAILING:**`).
///
/// VERDICT: the inversion is RIGHT, and the loop's *other* arm is exercised
/// here: a uid the index already holds while the destination file does not
/// exist (the state a crash between `add` and the document write leaves). The
/// author's tests only cover "uid free, document on disk".
#[test]
fn inverted_2_the_import_renames_when_only_the_uid_is_taken() {
    let (dir, mut store) = temp_store();
    let taken = store.add(PrfItem::local("Rabc", "existing"));
    assert_eq!(taken, "Rabc");
    assert!(
        !store.paths().profiles_dir().join("Rabc.yaml").exists(),
        "the fixture needs the uid taken and the document absent"
    );

    let source = dir.path().join("foreign");
    let item = PrfItem::local("Rabc", "imported");
    foreign_installation(
        &source,
        &Index {
            current: None,
            chain: Vec::new(),
            items: vec![item],
        },
        &[("Rabc.yaml", "IMPORTED\n")],
    );

    let report = store.import_from(&source).unwrap();
    assert_eq!(
        report.renamed, 1,
        "the uid collision did not fire: {report:?}"
    );
    assert_eq!(store.items().len(), 2);
    let imported = store.items().last().unwrap();
    assert_ne!(imported.uid, "Rabc");
    assert_eq!(
        store.read_document(imported).unwrap(),
        "IMPORTED\n",
        "the document must land under the renamed uid"
    );
    assert_eq!(
        store.get("Rabc").map(PrfItem::label),
        Some("existing"),
        "the first entry is untouched"
    );
}

/// CLAIM: the orphan check tests the destination, so an unindexed document is
/// never overwritten.
///
/// VERDICT: confirmed, and this test goes one step past the author's by also
/// asserting *where* the imported document landed and that the orphan's bytes
/// are untouched.
#[test]
fn inverted_2b_the_orphan_document_is_left_alone_and_the_import_lands_elsewhere() {
    let (dir, mut store) = temp_store();
    let orphan = store.paths().profiles_dir().join("Rabc.yaml");
    std::fs::write(&orphan, "KEEP ME\n").unwrap();

    let source = dir.path().join("foreign");
    let mut item = PrfItem::local("Rabc", "imported");
    // The source index says the document is called something else, which is the
    // hand-edited installation the orphan check exists for.
    item.file = Some("Rabc-source.yaml".to_owned());
    foreign_installation(
        &source,
        &Index {
            current: None,
            chain: Vec::new(),
            items: vec![item],
        },
        &[("Rabc-source.yaml", "IMPORTED\n")],
    );

    let report = store.import_from(&source).unwrap();
    assert_eq!(report.renamed, 1, "{report:?}");
    assert_eq!(std::fs::read_to_string(&orphan).unwrap(), "KEEP ME\n");
    let imported = store.items().last().unwrap();
    assert_ne!(imported.uid, "Rabc");
    assert_eq!(
        store.read_document(imported).unwrap(),
        "IMPORTED\n",
        "the source document must land under the new uid, not be lost"
    );
}

/// CLAIM: "`relay` and `smart` group types are refused".
///
/// `recheck.rs::f11_relay_is_still_accepted_although_this_core_removed_it` is an
/// inverted assertion (its doc comment still says `**FAILING:**`).
///
/// VERDICT: the inversion is RIGHT. Live core, `v1.19.31`:
///
/// ```text
/// $ verge-mihomo -t -d <dir> -f <file>
/// type: relay   -> test failed   ... was removed, please using dialer-proxy instead
/// type: smart   -> test failed   ... unsupported proxy group type: smart
/// type: select  -> test is successful
/// ```
///
/// The author's test checks `relay` only. This one checks both refused types and
/// all four accepted ones, and that the hint names none of the refused ones.
#[test]
fn inverted_3_relay_and_smart_are_refused_and_the_real_types_are_not() {
    for kind in ["relay", "smart"] {
        let text = format!(
            "mixed-port: 7890\n\
             proxies:\n  - {{ name: p, type: socks5, server: 1.2.3.4, port: 1080 }}\n\
             proxy-groups:\n  - {{ name: g, type: {kind}, proxies: [p, DIRECT] }}\n\
             rules:\n  - MATCH,g\n"
        );
        let c = config(&text);
        let report = validate::check(&c);
        let diag = report
            .diagnostics
            .iter()
            .find(|d| d.code == "E-GROUP-TYPE")
            .unwrap_or_else(|| panic!("`{kind}` was accepted: {}", report.render()));
        assert!(diag.message.contains(kind), "{}", diag.message);
        let hint = diag.hint.clone().unwrap_or_default();
        for bad in ["relay", "smart"] {
            assert!(
                !hint.contains(bad),
                "the hint recommends `{bad}`, which the core refuses: {hint}"
            );
        }
        assert!(!report.is_ok(), "`{kind}` must be an error");
    }

    for kind in ["select", "url-test", "fallback", "load-balance"] {
        let text = format!(
            "mixed-port: 7890\n\
             proxies:\n  - {{ name: p, type: socks5, server: 1.2.3.4, port: 1080 }}\n\
             proxy-groups:\n  - {{ name: g, type: {kind}, proxies: [p, DIRECT] }}\n\
             rules:\n  - MATCH,g\n"
        );
        let codes = error_codes(&config(&text));
        assert!(
            codes.is_empty(),
            "type {kind} is a real group type: {codes:?}"
        );
    }
}

/// CLAIM: "Wire types accept `null` as the empty list for `history`, `proxies`,
/// `rules`, `chains`, `providerChains`, the two provider maps, and
/// `RuleProviderInfo.payload`; metadata ports accept a string or a number."
///
/// VERDICT: confirmed, and generated rather than enumerated: for every wire
/// type below, *every* field whose value is a list or a map in a realistic
/// sample is set to `null` in turn and the sample must still parse. The author's
/// test names eight fields by hand.
#[test]
fn inverted_4_every_list_or_map_field_of_every_wire_type_reads_null() {
    /// Set each list/map field of `sample` to `null` in turn and parse.
    fn probe<T: serde::de::DeserializeOwned>(name: &str, sample: Value) {
        probe_impl::<T>(name, sample, true);
    }

    /// The same, for a sample that has no list or map field at all.
    fn probe_scalars_only<T: serde::de::DeserializeOwned>(name: &str, sample: Value) {
        probe_impl::<T>(name, sample, false);
    }

    fn probe_impl<T: serde::de::DeserializeOwned>(
        name: &str,
        sample: Value,
        needs_a_collection: bool,
    ) {
        let Value::Object(fields) = sample else {
            panic!("{name}: the sample must be an object");
        };
        let mut checked = 0usize;
        for (key, value) in &fields {
            if !(value.is_array() || value.is_object()) {
                continue;
            }
            let mut probe = fields.clone();
            probe.insert(key.clone(), Value::Null);
            let parsed = serde_json::from_value::<T>(Value::Object(probe));
            assert!(
                parsed.is_ok(),
                "{name}: `{key}: null` does not parse: {}",
                parsed.err().map(|e| e.to_string()).unwrap_or_default()
            );
            checked += 1;
        }
        if needs_a_collection {
            assert!(checked > 0, "{name}: the sample has no list or map field");
        }
    }

    probe::<ProxyView>(
        "ProxyView",
        json!({
            "alive": true, "dialer-proxy": "", "extra": {}, "history": [],
            "id": "fae47b09-9950-4ee3-be48-fb04f83906d7", "interface": "",
            "mptcp": false, "name": "DIRECT", "provider-name": "", "routing-mark": 0,
            "smux": false, "tfo": false, "type": "Direct", "udp": true, "uot": false,
            "xudp": false
        }),
    );
    probe::<ProxyView>(
        "ProxyView (group)",
        json!({
            "alive": true, "all": ["node-a", "DIRECT"], "dialer-proxy": "",
            "emptyFallback": "COMPATIBLE", "extra": {}, "hidden": false, "history": [],
            "icon": "", "interface": "", "mptcp": false, "name": "grp-select",
            "now": "node-a", "provider-name": "", "routing-mark": 0, "smux": false,
            "testUrl": "", "tfo": false, "type": "Selector", "udp": false, "uot": false,
            "xudp": false
        }),
    );
    probe::<ProxyProviderInfo>(
        "ProxyProviderInfo",
        json!({
            "name": "default", "type": "Proxy", "vehicleType": "Compatible",
            "proxies": [{"name": "node-a"}], "testUrl": "", "expectedStatus": "*",
            "updatedAt": "0001-01-01T00:00:00Z"
        }),
    );
    probe::<RuleInfo>(
        "RuleInfo",
        json!({"type": "DomainSuffix", "payload": "google.com", "proxy": "PROXY",
               "size": -1, "extra": {}}),
    );
    probe::<RuleProviderInfo>(
        "RuleProviderInfo",
        json!({"behavior": "Domain", "format": "YamlRule", "name": "p", "ruleCount": 3,
               "type": "Rule", "vehicleType": "HTTP", "updatedAt": "0001-01-01T00:00:00Z",
               "payload": ["DOMAIN,a.com"]}),
    );
    probe::<Connection>(
        "Connection",
        json!({
            "id": "c1", "metadata": {"network": "tcp", "type": "HTTP", "sourceIP": "1.2.3.4",
            "destinationIP": "5.6.7.8", "sourcePort": "50130", "destinationPort": "443",
            "host": "example.com", "dnsMode": "normal", "processPath": "", "specialProxy": "",
            "specialRules": "", "remoteDestination": "", "sniffHost": "", "uid": 0,
            "inboundName": "", "inboundPort": "0", "inboundUser": ""},
            "chains": [], "providerChains": [], "rule": "", "rulePayload": "",
            "start": "2026-09-25T00:00:00Z", "upload": 1, "download": 2,
            "extra": {}, "process": ""
        }),
    );
    probe_scalars_only::<Metadata>(
        "Metadata (ports are scalars, so this sample has no list to null)",
        json!({"network": "tcp", "type": "HTTP", "sourceIP": "1.2.3.4",
               "destinationIP": "5.6.7.8", "sourcePort": "50130", "destinationPort": "443",
               "host": "example.com", "dnsMode": "normal", "processPath": "",
               "specialProxy": "", "specialRules": "", "remoteDestination": "",
               "sniffHost": "", "uid": 0, "inboundName": "", "inboundPort": "0",
               "inboundUser": ""}),
    );

    // The wrapper types, through their `Default`.
    probe::<ProxiesResponse>("ProxiesResponse", json!({"proxies": {"a": {}}}));
    probe::<GroupsResponse>("GroupsResponse", json!({"proxies": []}));
    probe::<RulesResponse>("RulesResponse", json!({"rules": []}));
    probe::<ProxyProvidersResponse>("ProxyProvidersResponse", json!({"providers": {}}));
    probe::<RuleProvidersResponse>("RuleProvidersResponse", json!({"providers": {}}));
    probe::<cvt_core::mihomo::types::ConnectionsResponse>(
        "ConnectionsResponse",
        json!({"downloadTotal": 0, "uploadTotal": 0, "connections": [],
               "memory": 0}),
    );

    // Ports: a number, a string, a null and an absent field are one port.
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
    let absent: Metadata = serde_json::from_value(json!({})).unwrap();
    assert_eq!(absent.source_port, "", "an absent port is an empty port");
}

/// CLAIM: "`append_items` recomputes the catch-all position for every item, so
/// an append list holding a normal rule and a catch-all lands both. An append
/// list naming **two** catch-alls is refused by `validate`."
///
/// `recheck.rs::f13_one_append_list_holding_a_rule_and_a_catch_all_keeps_at_most_one`
/// is an inverted assertion (its doc comment still says `**FAILING:**`).
///
/// VERDICT: the inversion is RIGHT for the two-item case, and the refusal is
/// RIGHT for the two-catch-all case — with one caveat about *why* it is refused,
/// which `vocab_2` below records: the validator's `E-UNREACHABLE-RULES` is an
/// error for a document the core loads.
///
/// The author's `f13_an_append_of_two_items_either_lands_whole_or_is_refused`
/// covers two items. This one is generated over *zero to three* items and adds
/// two invariants the author's does not state: the base's own order is preserved
/// (a subsequence check), and the result is idempotent.
#[test]
fn inverted_5_append_lands_every_named_item_or_refuses() {
    fn run(base: &[String], items: &[String]) -> (Value, Option<Vec<String>>) {
        let doc = json!({"rules": base});
        let o = Overlay {
            append: BTreeMap::from([(
                "rules".to_owned(),
                items.iter().map(|s| json!(s)).collect::<Vec<Value>>(),
            )]),
            ..Overlay::default()
        };
        let mut d = doc.clone();
        if let Err(e) = o.apply(&mut d) {
            assert_eq!(d, doc, "a refused append touched the document: {e}");
            return (d, None);
        }
        let after = rules_of(&d);
        let mut twice = d.clone();
        o.apply(&mut twice).unwrap();
        assert_eq!(twice, d, "a second append changed the list");
        (d, Some(after))
    }

    // The two cases the fix targets, spelled out.
    let (doc, after) = run(
        &["DOMAIN,a.test,PROXY".to_owned(), "MATCH,DIRECT".to_owned()],
        &["DOMAIN,b.test,PROXY".to_owned(), "MATCH,REJECT".to_owned()],
    );
    assert_eq!(
        after.unwrap(),
        vec!["DOMAIN,a.test,PROXY", "DOMAIN,b.test,PROXY", "MATCH,REJECT"],
        "{doc}"
    );
    let (_, after) = run(
        &["DOMAIN,a.test,PROXY".to_owned(), "MATCH,DIRECT".to_owned()],
        &["MATCH,REJECT".to_owned(), "MATCH,DIRECT".to_owned()],
    );
    assert!(
        after.is_none(),
        "two catch-alls in one append list must be refused"
    );
}

proptest! {
    #![proptest_config(cases(1500))]

    /// The generated form of the append claim: for every base list and every
    /// append list of zero to three rules, either every named item is in the
    /// result (or was already there) and the base's order survives, or the patch
    /// is refused and the document is untouched.
    #[test]
    fn inverted_5b_append_lands_every_named_item_or_refuses(
        base in prop::collection::vec(
            prop::sample::select(vec![
                "MATCH,DIRECT", "MATCH,REJECT", "DOMAIN,a.test,PROXY",
                "DOMAIN-SUFFIX,b.test,DIRECT", "FINAL,PROXY", "DOMAIN,c.test,PROXY",
            ]),
            0..5,
        ),
        items in prop::collection::vec(
            prop::sample::select(vec![
                "MATCH,DIRECT", "MATCH,REJECT", "DOMAIN,d.test,PROXY", "FINAL,PROXY",
            ]),
            0..4,
        ),
    ) {
        let base: Vec<String> = base.iter().map(|s| (*s).to_owned()).collect();
        let items: Vec<String> = items.iter().map(|s| (*s).to_owned()).collect();
        let doc = json!({"rules": base});
        let o = Overlay {
            append: BTreeMap::from([(
                "rules".to_owned(),
                items.iter().map(|s| json!(s)).collect::<Vec<Value>>(),
            )]),
            ..Overlay::default()
        };
        let mut d = doc.clone();
        let Ok(_log) = o.apply(&mut d) else {
            prop_assert_eq!(d, doc, "a refused append touched the document");
            return Ok(());
        };
        let after = rules_of(&d);

        for item in &items {
            prop_assert!(
                base.contains(item) || after.contains(item),
                "`{}` was named in the append list but is in neither the base nor {:?}",
                item,
                after
            );
        }

        // The base's own items keep their relative order: an append inserts.
        // One exception is the documented one — an appended catch-all *replaces*
        // the base's first catch-all rather than stacking above it, so that item
        // may be gone.
        let first_terminal = base.iter().position(|r| is_terminal(r));
        let kept: Vec<&String> = base
            .iter()
            .enumerate()
            .filter(|(i, _)| Some(*i) != first_terminal)
            .map(|(_, r)| r)
            .collect();
        let mut cursor = 0usize;
        for item in kept {
            let found = after[cursor..].iter().position(|a| a == item);
            prop_assert!(
                found.is_some(),
                "the base's item `{}` is missing or reordered: {:?} -> {:?}",
                item,
                base,
                after
            );
            cursor += found.unwrap() + 1;
        }

        // A list that already held a catch-all may not gain a second one.
        let terminals = |list: &[String]| list.iter().filter(|r| is_terminal(r)).count();
        if terminals(&base) >= 1 {
            prop_assert!(
                terminals(&after) <= terminals(&base),
                "the catch-all count grew: {:?} -> {:?}",
                base,
                after
            );
        }

        let mut twice = d.clone();
        o.apply(&mut twice).unwrap();
        prop_assert_eq!(twice, d, "the append is not idempotent");
    }
}

// ==========================================================================
// 2. The security fix: `is_plain_component` guards one field of one path
// ==========================================================================

/// CLAIM: "`ProfileStore::import_from` now replaces any uid that is not one
/// plain path component (`is_plain_component`), both at the import and inside
/// `add`."
///
/// VERDICT: confirmed for `uid`. Every hostile shape below leaves the index
/// byte-identical and writes nothing outside `profiles/`. The author's tests
/// cover `uid: "../profiles"`; this is the generated table, and it also asserts
/// that the *landed* entry's document path is inside `profiles/`.
#[test]
fn security_1_no_uid_shape_writes_outside_the_profiles_directory() {
    let shapes: &[&str] = &[
        "../profiles",
        "../profiles.yaml",
        "..",
        ".",
        "",
        "a/b",
        "a\\b",
        "/etc/passwd",
        "./x",
        "profiles/../profiles",
        "..yaml",
        "...",
        "a/../../b",
        "\u{2024}\u{2024}/x",
        "\u{0}",
        "a\u{0}b",
        "a\nb",
    ];

    for shape in shapes {
        let dir = TempDir::new().unwrap();
        let paths = AppPaths::new(dir.path());
        paths.ensure_dirs().unwrap();
        let mut store = ProfileStore::load(&paths).unwrap();
        store.add(PrfItem::local("L1", "mine"));
        store.save().unwrap();
        let index_before = std::fs::read_to_string(paths.profiles_index()).unwrap();

        let source = dir.path().join("foreign");
        let mut item = PrfItem::local(*shape, "hostile");
        item.file = Some("evil.yaml".to_owned());
        foreign_installation(
            &source,
            &Index {
                current: None,
                chain: Vec::new(),
                items: vec![item],
            },
            &[("evil.yaml", "IMPORTED\n")],
        );

        let before = snapshot(dir.path());
        let report = store.import_from(&source).unwrap();
        let after = snapshot(dir.path());

        for (path, contents) in &after {
            if before.get(path) == Some(contents) {
                continue;
            }
            assert!(
                path.starts_with("profiles/"),
                "uid {shape:?} made the import touch `{path}` ({report:?})"
            );
        }
        assert_eq!(
            std::fs::read_to_string(paths.profiles_index()).unwrap(),
            index_before,
            "uid {shape:?} rewrote the index"
        );
        assert_eq!(report.documents_copied, 1, "uid {shape:?}: {report:?}");

        let landed = store.items().last().unwrap();
        let dest = document_path(&paths, landed);
        assert!(
            dest.starts_with(paths.profiles_dir()),
            "uid {shape:?} landed at {}",
            dest.display()
        );
        assert_eq!(store.read_document(landed).unwrap(), "IMPORTED\n");
    }
}

/// CLAIM: the guard is in `add` as well as at the import — "the funnel every
/// caller goes through".
///
/// A caller that builds a `PrfItem` itself (the CLI does, for a `local` profile)
/// must not be able to name a document outside `profiles/` through either the
/// uid or the pre-set `file`.
#[test]
fn security_3_add_itself_normalises_a_uid_and_a_file_name() {
    let (_dir, mut store) = temp_store();
    for uid in ["../profiles", "a/b", "..", "", "/etc/passwd"] {
        let mut item = PrfItem::local(uid, "x");
        item.file = Some("../../outside.yaml".to_owned());
        let assigned = store.add(item);
        assert!(
            assigned != ".." && !assigned.contains('/') && !assigned.is_empty(),
            "uid {uid:?} was honoured as {assigned:?}"
        );
        let stored = store.get(&assigned).unwrap();
        assert_eq!(stored.file_name(), format!("{assigned}.yaml"));
        let dest = document_path(store.paths(), stored);
        assert!(
            dest.starts_with(store.paths().profiles_dir()),
            "uid {uid:?} landed at {}",
            dest.display()
        );
    }

    // A plain uid is honoured, and a file name that disagrees with it is
    // normalised to match.
    let mut item = PrfItem::local("Rabc", "x");
    item.file = Some("../../outside.yaml".to_owned());
    assert_eq!(store.add(item), "Rabc");
    assert_eq!(store.get("Rabc").unwrap().file_name(), "Rabc.yaml");

    // A uid the index already holds is reassigned rather than honoured (F7).
    assert_ne!(store.add(PrfItem::local("Rabc", "y")), "Rabc");
}

/// CLAIM (the fix's own scope): the document is written to the path the import
/// chose for it.
///
/// VERDICT: **not fixed as a class.** `uid` is now one plain component, but
/// `PrfItem::file` — the *other* field of the same index entry that ends up in a
/// path — is used to build the *source* path, `source/profiles/<file>`, and it is
/// never checked. `Path::join` with an absolute path discards the base, so
/// `file: /home/user/.ssh/id_rsa` reads any file the process can read and copies
/// it into the profiles directory; `file: ../../secret.yaml` climbs out the same
/// way. The foreign index is the untrusted input in this code path.
///
/// Minimal input: a source index with `file: <absolute path of a file outside
/// the source directory>`.
#[test]
fn defect_1_the_source_file_field_still_reads_outside_the_source_directory() {
    let dir = TempDir::new().unwrap();
    let mut failures: Vec<String> = Vec::new();

    // Two ways to leave the source directory: an absolute path (`Path::join`
    // discards the base) and a climb.
    for label in ["absolute", "climb"] {
        let root = dir.path().join(format!("case-{label}"));
        let paths = AppPaths::new(&root);
        paths.ensure_dirs().unwrap();
        let mut store = ProfileStore::load(&paths).unwrap();
        let source = root.join("foreign");
        std::fs::create_dir_all(source.join("profiles")).unwrap();

        let canary = if label == "absolute" {
            root.join("canary.yaml")
        } else {
            source.join("canary.yaml")
        };
        std::fs::write(&canary, "CANARY\n").unwrap();
        let file = if label == "absolute" {
            canary.to_string_lossy().into_owned()
        } else {
            "../canary.yaml".to_owned()
        };

        let mut item = PrfItem::local("Rabc", "imported");
        item.file = Some(file.clone());
        foreign_installation(
            &source,
            &Index {
                current: None,
                chain: Vec::new(),
                items: vec![item],
            },
            &[("unused.yaml", "unused\n")],
        );

        let report = store.import_from(&source).unwrap();
        let landed = store.items().last().unwrap();
        let body = store.read_document(landed).unwrap_or_default();
        if report.documents_copied == 1 && body.contains("CANARY") {
            failures.push(format!(
                "{label}: `file: {file}` read {} and copied it into profiles/{}",
                canary.display(),
                landed.file_name()
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "the source file name is still used to read outside the source directory:\n{}",
        failures.join("\n")
    );
}

/// The other half of the same claim: a uid that *is* one plain component must be
/// honoured, not renamed. A fix that reassigned every uid would pass the
/// traversal test above and break the import.
#[test]
fn security_2_a_uid_that_is_one_plain_component_is_honoured() {
    for uid in [
        "Rabc",
        "a b",
        "\u{65e5}\u{672c}\u{8a9e}",
        "...",
        "..yaml",
        "a\nb",
        "R1.yaml",
    ] {
        let dir = TempDir::new().unwrap();
        let paths = AppPaths::new(dir.path());
        paths.ensure_dirs().unwrap();
        let mut store = ProfileStore::load(&paths).unwrap();

        let source = dir.path().join("foreign");
        let mut item = PrfItem::local(uid, "imported");
        item.file = Some("evil.yaml".to_owned());
        foreign_installation(
            &source,
            &Index {
                current: None,
                chain: Vec::new(),
                items: vec![item],
            },
            &[("evil.yaml", "IMPORTED\n")],
        );

        let report = store.import_from(&source).unwrap();
        assert_eq!(
            report.renamed, 0,
            "uid {uid:?} is one plain component: {report:?}"
        );
        let landed = store
            .get(uid)
            .unwrap_or_else(|| panic!("uid {uid:?} was renamed"));
        assert_eq!(landed.file_name(), format!("{uid}.yaml"));
        assert_eq!(store.read_document(landed).unwrap(), "IMPORTED\n");
        assert!(document_path(&paths, landed).starts_with(paths.profiles_dir()));
    }
}

/// CLAIM: the traversal class is closed because a uid is one plain component.
///
/// VERDICT: **not fixed as a class.** `document_path` joins
/// `profiles_dir` with `item.file_name()`, which is `item.file` verbatim when
/// the index sets it. `ProfileStore::load` validates nothing, so an index that a
/// hand edit, another front-end, a restored backup or a synced dotfiles
/// directory leaves behind decides where the *next* document write goes.
/// `profile/source.rs::store_fetched` is exactly such a write: it puts the body
/// of a fetched subscription at that path.
///
/// Minimal input: `profiles.yaml` holding `file: ../outside.yaml`.
#[test]
fn defect_2_a_loaded_index_entry_writes_its_document_outside_profiles() {
    let dir = TempDir::new().unwrap();
    let paths = AppPaths::new(dir.path());
    paths.ensure_dirs().unwrap();
    let outside = dir.path().join("outside.yaml");
    std::fs::write(&outside, "ORIGINAL\n").unwrap();

    let mut item = PrfItem::local("L1", "mine");
    item.file = Some("../outside.yaml".to_owned());
    let index = Index {
        current: Some("L1".to_owned()),
        chain: Vec::new(),
        items: vec![item],
    };
    std::fs::write(
        paths.profiles_index(),
        serde_norway::to_string(&index).unwrap(),
    )
    .unwrap();

    let store = ProfileStore::load(&paths).unwrap();
    let stored = store.get("L1").unwrap().clone();
    // The exact call `SubscriptionFetcher::update` makes with a fetched body.
    store.write_document(&stored, "FETCHED BODY\n").unwrap();

    assert_eq!(
        std::fs::read_to_string(&outside).unwrap(),
        "ORIGINAL\n",
        "a profile document was written to {}",
        outside.display()
    );
}

/// The same field, the other direction: `remove` deletes through
/// `document_path`, so a loaded index entry can delete an arbitrary file.
#[test]
fn defect_3_remove_deletes_through_a_loaded_index_entry_outside_profiles() {
    let dir = TempDir::new().unwrap();
    let paths = AppPaths::new(dir.path());
    paths.ensure_dirs().unwrap();
    let outside = dir.path().join("precious.txt");
    std::fs::write(&outside, "PRECIOUS\n").unwrap();

    let mut item = PrfItem::local("L1", "mine");
    item.file = Some("../precious.txt".to_owned());
    let index = Index {
        current: None,
        chain: Vec::new(),
        items: vec![item],
    };
    std::fs::write(
        paths.profiles_index(),
        serde_norway::to_string(&index).unwrap(),
    )
    .unwrap();

    let mut store = ProfileStore::load(&paths).unwrap();
    store.remove("L1").unwrap();

    assert!(
        outside.exists(),
        "`remove` deleted {} — a file outside the profiles directory",
        outside.display()
    );
}

// ==========================================================================
// 3. `shape_conflict`: claimed symmetric and precise
// ==========================================================================

/// Apply the two halves of a `set`+`append` overlay the way `apply` would, twice,
/// and return the result. Panics if either half cannot be applied or if the two
/// together are not repeatable — which is the only case in which refusing the
/// combined overlay is justified.
fn halves_work(doc: &Value, set: (&str, &Value), append: (&str, &[Value])) -> Value {
    halves_work_and_the_set_survives(doc, set.0, set.1, append.0, append.1).unwrap_or_else(|| {
        panic!(
            "the halves must both apply, repeat unchanged, and keep the set's shape: \
             set `{}`, append `{}`",
            set.0, append.0
        )
    })
}

/// `Some(result)` when the two halves of a `set`+`append` overlay can both be
/// applied in `apply`'s order, repeated without change, **and** the value the
/// `set` wrote still has the shape it asked for afterwards.
///
/// That last clause is what makes the oracle sound. `set: {a: 1}` with
/// `append: {a: ["x"]}` also applies and repeats — but the append overwrites the
/// scalar, so refusing that pair is right. Only a refusal of a pair that works
/// *and* keeps both edits is a false rejection.
fn halves_work_and_the_set_survives(
    doc: &Value,
    set_path: &str,
    value: &Value,
    list_path: &str,
    items: &[Value],
) -> Option<Value> {
    let set_only = overlay(&[(set_path, value.clone())], &[]);
    let append_only = overlay(&[], &[(list_path, items.to_vec())]);
    let mut d = doc.clone();
    set_only.apply(&mut d).ok()?;
    append_only.apply(&mut d).ok()?;
    let once = d.clone();
    set_only.apply(&mut d).ok()?;
    append_only.apply(&mut d).ok()?;
    if d != once {
        return None;
    }
    let parsed = Path::parse(set_path).ok()?;
    let survived = match value {
        Value::Object(_) => path::get(&d, &parsed).is_some_and(Value::is_object),
        Value::Array(_) => path::get(&d, &parsed).is_some_and(Value::is_array),
        Value::Null => path::get(&d, &parsed).is_none_or(Value::is_null),
        scalar => path::get(&d, &parsed) == Some(scalar),
    };
    survived.then_some(d)
}

/// CLAIM: "a `set` that reaches *into* a list (`a.b[0]`, `a[name=x]`) is
/// allowed, one that must walk *through* it as a mapping (`a.b`) is refused, in
/// either order."
///
/// VERDICT: **claim over-strong.** Reaching into a list and then into the
/// *element* — `a[0].b`, `a[name=x].b` — is refused, although the element's
/// mapping is exactly what an index or a selector produces. `shape_conflict`
/// asks "does the longer path contain any `Key` after the shared prefix", but a
/// `Key` *after* an `Index`/`Selector` applies to the element, not to the list.
///
/// Minimal input: `set: {"a[0].b": 1}` with `append: {a: ["x"]}` over
/// `{"a": [{"b": 0}]}`. Both halves apply and repeat (that is asserted below
/// before the refusal is reported), so the combined overlay works.
#[test]
fn defect_4_shape_conflict_refuses_a_set_into_a_list_element() {
    let doc = json!({"a": [{"b": 0}]});
    let set = ("a[0].b", json!(1));
    let append = ("a", vec![json!("x")]);
    let expected = halves_work(&doc, (set.0, &set.1), (append.0, &append.1));
    assert_eq!(expected, json!({"a": [{"b": 1}, "x"]}));

    let combined = overlay(&[set], &[append]);
    let err = combined.validate().err().map(|e| e.to_string());
    assert!(
        err.is_none(),
        "`set: {{a[0].b: 1}}` with `append: {{a: [x]}}` applies and repeats, but was \
         refused: {}",
        err.unwrap_or_default()
    );

    let mut d = doc;
    combined.apply(&mut d).unwrap();
    assert_eq!(d, expected);
}

/// The same claim, the other direction: when the `set` path is the *shorter*
/// one, the value decides the shape, and `shape_conflict` ignores the value
/// entirely. A mapping value at the ancestor is consistent with a list at the
/// descendant path.
///
/// Minimal input: `set: {a: {x: 1}}` with `append: {a.b: ["y"]}`.
#[test]
fn defect_5_shape_conflict_refuses_a_mapping_set_at_an_ancestor() {
    let doc = json!({});
    let set = ("a", json!({"x": 1}));
    let append = ("a.b", vec![json!("y")]);
    let expected = halves_work(&doc, (set.0, &set.1), (append.0, &append.1));
    assert_eq!(expected, json!({"a": {"x": 1, "b": ["y"]}}));

    let combined = overlay(&[set], &[append]);
    let err = combined.validate().err().map(|e| e.to_string());
    assert!(
        err.is_none(),
        "`set: {{a: {{x: 1}}}}` with `append: {{a.b: [y]}}` applies and repeats, but was \
         refused: {}",
        err.unwrap_or_default()
    );

    let mut d = doc;
    combined.apply(&mut d).unwrap();
    assert_eq!(d, expected);
}

/// A generated form of the same claim: over random documents and random pairs of
/// *existing* paths in them, a `set`+`append` overlay may only be refused when
/// the two halves cannot be applied and repeated. Every refusal that survives
/// that test is a false rejection.
///
/// The generator walks the document it just built and emits every addressable
/// path, so `set` and `append` both resolve; the halves are then applied exactly
/// as `apply` would order them.
#[test]
fn shape_conflict_generated_only_refuses_overlays_that_cannot_apply() {
    let mut refusals: Vec<String> = Vec::new();
    let mut workable = 0usize;
    // A small deterministic corpus with the same shape the generator produces,
    // so the counterexamples do not depend on the random draw.
    let corpus: Vec<Value> = vec![
        json!({"a": [{"b": 0}]}),
        json!({"a": {"b": [0]}}),
        json!({"a": [{"name": "x", "b": [0]}]}),
        json!({"a": {"b": {"c": 1}}}),
        json!({"a": [{"b": {"c": [1]}}], "d": {"e": 2}}),
    ];
    for doc in &corpus {
        for (set_path, list_path) in [
            ("a[0].b", "a"),
            ("a[name=x].b", "a"),
            ("a", "a.b"),
            ("a.b", "a"),
            ("a.b.c", "a"),
            ("a.b[0]", "a.b"),
        ] {
            let value = json!(1);
            if halves_work_and_the_set_survives(doc, set_path, &value, list_path, &[json!("fresh")])
                .is_none()
            {
                continue;
            }
            workable += 1;
            let combined = overlay(
                &[(set_path, value.clone())],
                &[(list_path, vec![json!("fresh")])],
            );
            if let Err(e) = combined.validate() {
                refusals.push(format!(
                    "document {doc}: set `{set_path}` = {value} with append `{list_path}` \
                     applies and repeats, but is refused: {e}"
                ));
            }
        }
    }
    assert!(workable > 0, "the corpus produced nothing to check");
    assert!(
        refusals.is_empty(),
        "{} overlay(s) that apply and repeat were refused:\n{}",
        refusals.len(),
        refusals.join("\n")
    );
}

proptest! {
    #![proptest_config(cases(1200))]

    /// The generated attack on the same claim: build a document, collect every
    /// path in it, then ask for a `set` at one and an `append` at another. If the
    /// combined overlay is refused, the two halves must not both apply and
    /// repeat — otherwise the refusal is a false rejection.
    #[test]
    fn shape_conflict_generated_over_random_documents(
        doc in arb_doc(),
        picks in (0usize..64, 0usize..64),
        value in prop_oneof![
            Just(json!(1)),
            Just(json!("s")),
            Just(json!([1, 2])),
            Just(json!({"x": 1})),
            Just(Value::Null),
        ],
    ) {
        let paths = node_paths(&doc);
        if paths.len() < 2 {
            return Ok(());
        }
        let set_path = paths[picks.0 % paths.len()].clone();
        let list_path = paths[picks.1 % paths.len()].clone();
        let combined = overlay(&[(set_path.as_str(), value.clone())], &[(list_path.as_str(), vec![json!("fresh")])]);
        if combined.validate().is_ok() {
            return Ok(());
        }
        // Refusing is only wrong when both halves land, repeat, and the `set`'s
        // own shape survives the append.
        if halves_work_and_the_set_survives(&doc, &set_path, &value, &list_path, &[json!("fresh")])
            .is_none()
        {
            return Ok(());
        }
        prop_assert!(
            false,
            "refused an overlay whose halves apply and repeat: set `{}` = {}, append `{}`",
            set_path, value, list_path
        );
    }
}

/// A depth-limited document whose paths are all addressable.
fn arb_doc() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(json!(0)),
        Just(json!("s")),
        Just(Value::Null),
        Just(json!(true)),
    ];
    leaf.prop_recursive(3, 24, 3, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..3).prop_map(Value::Array),
            prop::collection::vec(
                (prop_oneof![Just("a"), Just("b"), Just("name")], inner),
                0..3
            )
            .prop_map(|pairs| Value::Object(
                pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()
            )),
        ]
    })
}

/// Every path that addresses a node of `value`, as the path language spells it.
fn node_paths(value: &Value) -> Vec<String> {
    fn walk(v: &Value, prefix: &str, out: &mut Vec<String>) {
        match v {
            Value::Object(map) => {
                for (k, child) in map {
                    let path = if prefix.is_empty() {
                        k.clone()
                    } else {
                        format!("{prefix}.{k}")
                    };
                    out.push(path.clone());
                    walk(child, &path, out);
                }
            }
            Value::Array(items) => {
                for (i, child) in items.iter().enumerate() {
                    let path = format!("{prefix}[{i}]");
                    out.push(path.clone());
                    if let Some(name) = child.get("name").and_then(Value::as_str) {
                        out.push(format!("{prefix}[name={name}]"));
                    }
                    walk(child, &path, out);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(value, "", &mut out);
    out
}

// ==========================================================================
// 4. `Overlay::apply` is atomic
// ==========================================================================

/// CLAIM: "`Overlay::apply` is atomic: it works on a clone and commits only on
/// success."
///
/// VERDICT: confirmed. The author's `a_failing_set_leaves_the_document_untouched`
/// is one input; this is generated over documents, removal lists, set paths and
/// list paths, and it also covers failures raised by `validate` (a
/// self-contradicting overlay) rather than only by `set`.
#[test]
fn atomic_1_a_refused_overlay_leaves_the_document_alone() {
    let paths: Vec<&str> = vec![
        "a",
        "a.b",
        "a[0]",
        "a[9]",
        "a[0].b",
        "a.b[0]",
        "a[name=x]",
        "a[name=zzz]",
        "a[name=x].b",
        "b.c",
        "b.c.d",
        "rules",
        "rules[1]",
        "rules[9]",
        "missing.deep.path",
    ];
    let mut checked = 0usize;
    let mut refused = 0usize;
    for doc in [
        json!({}),
        json!({"a": [{"name": "x", "b": 1}]}),
        json!({"a": "scalar", "rules": ["A,DIRECT", "MATCH,DIRECT"]}),
        json!({"a": {"b": [1, 2]}, "rules": []}),
    ] {
        for set_path in &paths {
            for list_path in &paths {
                let o = Overlay {
                    remove: vec!["a[9]".to_owned(), "missing.deep".to_owned()],
                    set: BTreeMap::from([((*set_path).to_owned(), json!(1))]),
                    prepend: BTreeMap::from([((*list_path).to_owned(), vec![json!("p")])]),
                    append: BTreeMap::from([((*list_path).to_owned(), vec![json!("q")])]),
                    ..Overlay::default()
                };
                let before = doc.clone();
                let mut d = doc.clone();
                checked += 1;
                if o.apply(&mut d).is_err() {
                    refused += 1;
                    assert_eq!(
                        d, before,
                        "a refused overlay left edits behind: set {set_path}, list {list_path}"
                    );
                }
            }
        }
    }
    assert!(checked > 100, "the corpus must be large: {checked}");
    assert!(
        refused > 0,
        "no overlay in the corpus was refused, so the test proves nothing"
    );
}

proptest! {
    #![proptest_config(cases(1200))]

    /// The same claim over generated documents and paths.
    #[test]
    fn atomic_2_apply_is_atomic_over_generated_inputs(
        doc in arb_doc(),
        set_path in prop::sample::select(vec![
            "a", "a.b", "a[0]", "a[9]", "a[0].b", "a.b[0]", "a[name=x]",
            "a[name=zzz]", "a[name=x].b", "missing.deep.path",
        ]),
        set_value in prop_oneof![
            Just(json!(1)),
            Just(json!([1, 2])),
            Just(json!({"x": 1})),
            Just(Value::Null),
        ],
        list_path in prop::sample::select(vec![
            "a", "a.b", "a[0]", "a.b[0]", "rules", "rules[9]", "missing.deep",
        ]),
    ) {
        let o = Overlay {
            remove: vec!["a[9]".to_owned(), "missing.deep".to_owned()],
            set: BTreeMap::from([(set_path.to_owned(), set_value)]),
            prepend: BTreeMap::from([(list_path.to_owned(), vec![json!("p")])]),
            append: BTreeMap::from([(list_path.to_owned(), vec![json!("q")])]),
            ..Overlay::default()
        };
        let before = doc.clone();
        let mut d = doc;
        if o.apply(&mut d).is_err() {
            prop_assert_eq!(d, before, "a refused overlay left edits behind");
        }
    }
}

/// What the atomicity test above would have caught, reconstructed: before the
/// fix, `apply` ran the operations in place, so an overlay whose second
/// operation fails keeps the first one's edit. Each operation is applied here as
/// its own overlay, in `apply`'s order, to show that the first succeeds and the
/// second fails — which is exactly the state the old code left behind.
#[test]
fn atomic_3_the_pre_fix_behaviour_is_reproducible_from_the_operations() {
    let doc = json!({"a": "scalar"});
    let first = overlay(&[("b", json!(1))], &[]);
    let second = overlay(&[("a.b", json!(2))], &[]);

    let mut d = doc.clone();
    first.apply(&mut d).expect("the first edit lands");
    assert_eq!(d, json!({"a": "scalar", "b": 1}));
    assert!(
        second.apply(&mut d).is_err(),
        "the second edit fails on a scalar parent"
    );
    assert_eq!(
        d,
        json!({"a": "scalar", "b": 1}),
        "an in-place apply would have kept the first edit after the second failed"
    );

    // The combined overlay is refused, and the document is byte-identical.
    let combined = Overlay {
        set: BTreeMap::from([("b".to_owned(), json!(1)), ("a.b".to_owned(), json!(2))]),
        ..Overlay::default()
    };
    let mut d = doc.clone();
    assert!(combined.apply(&mut d).is_err());
    assert_eq!(d, doc, "the fix must leave the document untouched");
}

// ==========================================================================
// 5. The core's vocabulary: the newest diagnostics against the real core
// ==========================================================================

/// CLAIM: "an unknown rule kind reports `W-RULE-KIND`".
///
/// VERDICT: **claim over-strong**: `IP-CIDR6` is a rule kind the core loads and
/// this build reports it as unknown, because `mihomo::types::RULE_KINDS` has no
/// entry for it — while `validate::check_rule_payload` special-cases `IP-CIDR6`
/// by name, so the omission is an oversight rather than a decision.
///
/// Live core, `v1.19.31`:
///
/// ```text
/// $ verge-mihomo -t -d <dir> -f <file>     # rules: [IP-CIDR6,2001:db8::/32,DIRECT]
/// configuration file <file> test is successful
/// ```
///
/// This test is table-driven over *every* rule kind the core loads: it asserts
/// that the only kind this build does not know is `IP-CIDR6`.
#[test]
fn defect_6_ip_cidr6_is_a_rule_kind_the_core_loads_but_this_build_warns_about() {
    // Every kind accepted by `verge-mihomo -t`, one payload each.
    let kinds: &[(&str, &str)] = &[
        ("DOMAIN", "a.com"),
        ("DOMAIN-SUFFIX", "a.com"),
        ("DOMAIN-KEYWORD", "a"),
        ("DOMAIN-REGEX", "^a"),
        ("DOMAIN-WILDCARD", "a.*"),
        ("GEOSITE", "category-ads-all"),
        ("GEOIP", "CN"),
        ("SRC-GEOIP", "CN"),
        ("IP-ASN", "13335"),
        ("SRC-IP-ASN", "13335"),
        ("IP-CIDR", "10.0.0.0/8"),
        ("IP-CIDR6", "2001:db8::/32"),
        ("SRC-IP-CIDR", "10.0.0.0/8"),
        ("IP-SUFFIX", "10.0.0.0/8"),
        ("SRC-IP-SUFFIX", "10.0.0.0/8"),
        ("SRC-PORT", "80"),
        ("DST-PORT", "80"),
        ("IN-PORT", "7890"),
        ("IN-USER", "x"),
        ("IN-NAME", "x"),
        ("IN-TYPE", "HTTP"),
        ("PROCESS-NAME", "curl"),
        ("PROCESS-PATH", "/usr/bin/curl"),
        ("PROCESS-NAME-REGEX", "^cu"),
        ("PROCESS-PATH-REGEX", "^/usr"),
        ("PROCESS-NAME-WILDCARD", "cu*"),
        ("PROCESS-PATH-WILDCARD", "/usr*"),
        ("REMATCH-NAME", "x"),
        ("NETWORK", "udp"),
        ("DSCP", "4"),
        ("UID", "1000"),
        ("AND", "((DOMAIN,a.com),(NETWORK,udp))"),
        ("OR", "((DOMAIN,a.com),(NETWORK,udp))"),
        ("NOT", "((DOMAIN,a.com))"),
    ];

    let mut warned: Vec<&str> = Vec::new();
    for (kind, payload) in kinds {
        let text = format!("mixed-port: 7890\nrules:\n  - {kind},{payload},DIRECT\n");
        let c = config(&text);
        if all_codes(&c).contains(&"W-RULE-KIND") {
            warned.push(kind);
        }
    }
    assert!(
        warned.is_empty(),
        "these rule kinds load in mihomo v1.19.31 and are reported as unknown: {warned:?}"
    );
}

/// CLAIM: "a field after a payload-less rule's policy reports
/// `W-MATCH-WITH-PAYLOAD` as a **warning**, never an error, because the core
/// loads such a line and ignores the field."
///
/// VERDICT: confirmed. Live core: `MATCH,DIRECT,no-resolve` and
/// `MATCH,DIRECT,src` both `test is successful`. This test also pins the two
/// neighbouring cases the author's tests do not: the warning must be present,
/// and the *bare* `MATCH` (which the core refuses) must be an error rather than
/// the same warning.
#[test]
fn vocab_1_a_field_after_a_policy_is_a_warning_and_a_bare_match_is_an_error() {
    for line in [
        "MATCH,DIRECT,no-resolve",
        "MATCH,DIRECT,src",
        "MATCH,DIRECT,1.2.3.4,no-resolve",
        "MATCH,PROXY,no-resolve",
    ] {
        let text = format!(
            "mixed-port: 7890\n\
             proxy-groups:\n  - {{ name: PROXY, type: select, proxies: [DIRECT] }}\n\
             rules:\n  - {line}\n"
        );
        let c = config(&text);
        let report = validate::check(&c);
        let codes: Vec<&str> = report.diagnostics.iter().map(|d| d.code).collect();
        assert!(
            codes.contains(&"W-MATCH-WITH-PAYLOAD"),
            "`{line}`: the ignored field is not advised about: {codes:?}"
        );
        assert_eq!(
            report.errors(),
            0,
            "`{line}` loads in the core, so it must not be an error: {}",
            report.render()
        );
    }

    // The bare `MATCH` is a different case: the core refuses it.
    let bare = config("mixed-port: 7890\nrules:\n  - MATCH\n");
    assert!(
        error_codes(&bare).contains(&"E-RULE-MALFORMED"),
        "{:?}",
        all_codes(&bare)
    );
}

/// CLAIM: "`FINAL` is not a rule kind and a bare `MATCH` is not a rule (both
/// refused by `Rule::parse`)".
///
/// VERDICT: confirmed. Live core, `v1.19.31`:
///
/// ```text
/// $ verge-mihomo -t -d <dir> -f <file>
/// rules: [FINAL,DIRECT]  -> test failed   rules[0] [FINAL,DIRECT] error: format invalid
/// rules: [MATCH]         -> test failed   rules[0] [MATCH] error: format invalid
/// rules: [MATCH,DIRECT]  -> test is successful
/// ```
///
/// `recheck.rs::f10_a_bare_match_is_refused_and_an_unknown_kind_is_reported` is
/// NOT an inversion — it already asserts the fixed behaviour — and it is right.
#[test]
fn vocab_2_final_and_a_bare_match_are_refused_by_the_core_and_by_the_parser() {
    for src in ["MATCH", "FINAL", "FINAL,PROXY", "FINAL,DIRECT"] {
        assert!(Rule::parse(src).is_none(), "`{src}` is not a rule");
    }
    assert!(Rule::parse("MATCH,DIRECT").is_some());

    let final_rule = config("mixed-port: 7890\nrules:\n  - FINAL,DIRECT\n");
    let codes = all_codes(&final_rule);
    assert!(codes.contains(&"E-RULE-MALFORMED"), "{codes:?}");
    assert!(
        !codes.contains(&"W-RULE-KIND"),
        "a line the parser refuses is not also a kind warning: {codes:?}"
    );

    // `FINAL,DIRECT,no-resolve` *does* parse (three fields), so it is the
    // unknown-kind warning — and the policy it invents is reported too.
    let three = config("mixed-port: 7890\nrules:\n  - FINAL,DIRECT,no-resolve\n");
    let codes = all_codes(&three);
    assert!(codes.contains(&"W-RULE-KIND"), "{codes:?}");
    assert!(codes.contains(&"E-DANGLING-POLICY"), "{codes:?}");
}

/// CLAIM: the validator's codes are errors only where the core refuses.
///
/// VERDICT: **claim violated, and the newest append fix is built on it.**
/// `E-UNREACHABLE-RULES` is an *error* for a second terminal rule, and
/// `Service::start_core` refuses to start a core when any error is present
/// (`service.rs:290-296`). Two catch-alls load in the core:
///
/// ```text
/// $ verge-mihomo -t -d <dir> -f <file>     # rules: [MATCH,DIRECT, MATCH,REJECT]
/// configuration file <file> test is successful
/// ```
///
/// so a configuration the core accepts is refused before the core ever sees it —
/// the exact failure mode `validate`'s own module doc calls the one mistake a
/// validator must not make ("The core will refuse to start, or behaviour is
/// undefined").
#[test]
fn defect_7_two_catch_alls_load_in_the_core_but_are_a_blocking_error_here() {
    let c = config("mixed-port: 7890\nrules:\n  - MATCH,DIRECT\n  - MATCH,REJECT\n");
    let report = validate::check(&c);
    assert!(
        report.is_ok(),
        "the core loads this document (`test is successful`), so it must not be an \
         error: {}",
        report.render()
    );
}

/// CLAIM: "An append list naming **two** catch-alls is refused by `validate`",
/// because only one of them could ever run.
///
/// VERDICT: **incomplete** — the same argument applies to `prepend`, which is
/// not refused. Prepending one catch-all puts it *above* every rule the
/// subscription wrote, including the subscription's own catch-all, so the whole
/// base list becomes dead. The append refusal exists precisely to avoid
/// producing that document; `prepend` still produces it.
#[test]
fn defect_8_prepending_a_catch_all_deadens_the_base_and_is_not_refused() {
    let o = Overlay::from_yaml("prepend:\n  rules: [\"MATCH,REJECT\"]\n");
    let Ok(o) = o else {
        return; // refusing it is a fix
    };
    let mut doc = json!({"rules": ["DOMAIN-SUFFIX,google.com,PROXY", "MATCH,DIRECT"]});
    o.apply(&mut doc).unwrap();
    let rules = rules_of(&doc);
    let terminals = rules.iter().filter(|r| is_terminal(r)).count();
    assert_eq!(
        terminals, 1,
        "the prepend left {terminals} catch-alls, and every rule below the first one \
         is dead: {rules:?}"
    );
}

/// The half of the `shape_conflict` claim that does hold: the genuine
/// disagreements are refused, in *both* orders, and the shapes that are
/// consistent are not. This is the control for the two failing tests above — it
/// shows they target a specific over-reach rather than "the check refuses
/// things".
#[test]
fn shape_conflict_still_refuses_the_genuine_disagreements_in_both_orders() {
    // A list edit makes the path a list; the set either needs a mapping there or
    // writes something that is not a list.
    for (set_path, set_value, list_path) in [
        ("a", json!("scalar"), "a.b"), // set at the ancestor, a scalar
        ("a", json!([1, 2]), "a.b"),   // set at the ancestor, a list
        ("a.b", json!("scalar"), "a"), // set below the list path
        ("a.b.c", json!(1), "a"),      // two keys below it
        ("a", json!("scalar"), "a"),   // the same path, and not a list
        ("a.b", json!(1), "a.b"),
    ] {
        let o = overlay(
            &[(set_path, set_value.clone())],
            &[(list_path, vec![json!("fresh")])],
        );
        let err = o.validate().err().map(|e| e.to_string());
        assert!(
            err.is_some(),
            "set `{set_path}` = {set_value} with append `{list_path}` was accepted"
        );
        // A refused overlay must not touch the document either.
        let mut doc = json!({"a": {"b": [1]}});
        let before = doc.clone();
        assert!(o.apply(&mut doc).is_err());
        assert_eq!(doc, before);
    }

    // Consistent shapes: reaching *into* a list with an index or a selector, an
    // equal path that is already a list, and unrelated paths.
    for (set_path, set_value, list_path) in [
        ("a[0]", json!(1), "a"),
        ("a[name=x]", json!(1), "a"),
        ("a", json!([1, 2]), "a"),
        ("a.b[0]", json!(1), "a.b"),
        ("b", json!(1), "c"),
    ] {
        let o = overlay(
            &[(set_path, set_value)],
            &[(list_path, vec![json!("fresh")])],
        );
        let err = o.validate().err().map(|e| e.to_string());
        assert!(
            err.is_none(),
            "set `{set_path}` with append `{list_path}` is consistent but was refused: {}",
            err.unwrap_or_default()
        );
    }
}

/// Two observations, not claims: shapes the core **refuses** that this build
/// reports nothing about. Neither is a false rejection, so neither is a defect
/// against any claim the author made — they are recorded because the project's
/// other advertised property is "no false negatives", which covers them.
///
/// Live core, `v1.19.31`:
///
/// ```text
/// $ verge-mihomo -t -d <dir> -f <file>
/// rules: [DOMAIN-SUFFIX,,DIRECT]      -> test failed
/// rules: ["DOMAIN,a.com,DIRECT", 123] -> test failed
/// ```
///
/// * the first parses here (`Rule::parse` accepts an empty payload) so
///   `E-RULE-MALFORMED` never fires;
/// * the second is dropped before the validator sees it: `Config::raw_rules`
///   collects only the string entries of the list, so a non-string entry the
///   user wrote is invisible to every diagnostic.
#[test]
fn observation_two_shapes_the_core_refuses_that_nothing_reports() {
    let empty_payload = config("mixed-port: 7890\nrules:\n  - DOMAIN-SUFFIX,,DIRECT\n");
    assert!(Rule::parse("DOMAIN-SUFFIX,,DIRECT").is_some());
    assert!(
        !all_codes(&empty_payload).contains(&"E-RULE-MALFORMED"),
        "this records the current behaviour"
    );

    let numeric_entry =
        config("mixed-port: 7890\nrules:\n  - DOMAIN,a.com,DIRECT\n  - 123\n  - MATCH,DIRECT\n");
    assert_eq!(
        numeric_entry.raw_rules().len(),
        2,
        "the number is dropped before any diagnostic can see it"
    );
    let report = validate::check(&numeric_entry);
    assert!(
        report.errors() == 0,
        "this records the current behaviour: {}",
        report.render()
    );
}

// ==========================================================================
// 6. Seams
// ==========================================================================

/// `path::remove` with a `Selector`: every match goes, and the removal repeats.
///
/// The author's `f9_a_named_removal_is_idempotent_and_a_selector_rename_is_safe`
/// checks one three-element list. This is generated over lists whose names
/// collide to any degree, including lists where *every* element matches.
#[test]
fn selector_removal_removes_every_match_and_repeats() {
    for names in [
        vec!["A", "A", "B"],
        vec!["A", "A", "A"],
        vec!["B", "A"],
        vec!["B", "C"],
        vec![],
    ] {
        let proxies: Vec<Value> = names.iter().map(|n| json!({"name": n})).collect();
        let mut doc = json!({"proxies": proxies});
        let o = Overlay::from_yaml("remove: [\"proxies[name=A]\"]\n").unwrap();
        o.apply(&mut doc).unwrap();
        let left: Vec<&str> = doc["proxies"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v["name"].as_str())
            .collect();
        assert!(
            !left.contains(&"A"),
            "a removal that names its target left one behind: {names:?} -> {left:?}"
        );
        let once = doc.clone();
        o.apply(&mut doc).unwrap();
        assert_eq!(doc, once, "the removal is not repeatable: {names:?}");
    }
}

proptest! {
    #![proptest_config(cases(600))]

    /// The same, generated.
    #[test]
    fn selector_removal_removes_every_match_over_generated_lists(
        names in prop::collection::vec(prop::sample::select(vec!["A", "B", "C"]), 0..6),
    ) {
        let proxies: Vec<Value> = names.iter().map(|n| json!({"name": n})).collect();
        let mut doc = json!({"proxies": proxies});
        let o = Overlay::from_yaml("remove: [\"proxies[name=A]\"]\n").unwrap();
        o.apply(&mut doc).unwrap();
        prop_assert!(
            doc["proxies"]
                .as_array()
                .unwrap()
                .iter()
                .all(|v| v["name"].as_str() != Some("A")),
            "a match survived: {:?} -> {}",
            names,
            doc["proxies"]
        );
        let once = doc.clone();
        o.apply(&mut doc).unwrap();
        prop_assert_eq!(doc, once);
    }
}

/// The working half of the **uncommitted** control-plane change in
/// `enhance/pipeline.rs` (the tree is not clean at `67aa1ea`; see the report).
/// A control-plane key the *base* declares is restored when an enhancement
/// changes it, and the user is told.
#[test]
fn control_plane_1_a_key_the_base_declares_is_restored_and_reported() {
    let mut f = Fixture::new();
    f.base();
    f.add(
        PrfItem::patch("m1", "merge", ProfileType::Merge),
        "external-controller: 10.0.0.1:9999\n",
    );
    let outcome = f.pipeline.generate(&f.store).unwrap();
    assert_eq!(
        outcome.config.get_str("external-controller").as_deref(),
        Some("127.0.0.1:9090"),
        "the base owns the endpoint"
    );
    assert!(
        outcome
            .warnings
            .iter()
            .any(|w| w.contains("external-controller")),
        "the user must be told: {:?}",
        outcome.warnings
    );
}

/// The gap in the same uncommitted change: the restore iterates over the keys the
/// *base* declares, so an enhancement that **introduces** a control-plane key the
/// base omitted keeps it, with no warning. `Service::endpoint()` reads the
/// endpoint out of the generated document, which is what makes this the same
/// door the change is trying to close.
#[test]
fn defect_9_an_enhancement_can_introduce_a_control_plane_key_the_base_omits() {
    let mut f = Fixture::new();
    f.base(); // declares external-controller, and no secret and no CORS block
    f.add(
        PrfItem::patch("m1", "merge", ProfileType::Merge),
        "secret: attacker\nexternal-controller-cors:\n  allow-origins: ['*']\n",
    );
    let outcome = f.pipeline.generate(&f.store).unwrap();
    let doc = outcome.config.as_value();
    assert!(
        doc.get("secret").is_none(),
        "an enhancement introduced `secret` and nothing restored it; warnings: {:?}",
        outcome.warnings
    );
    assert!(
        doc.get("external-controller-cors").is_none(),
        "an enhancement widened CORS to `*` and nothing restored it; warnings: {:?}",
        outcome.warnings
    );
}

struct Fixture {
    _dir: TempDir,
    store: ProfileStore,
    pipeline: Pipeline,
}

impl Fixture {
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
}

/// A last guard on the helper the security tests rely on: `snapshot` must see
/// files the store writes, or those tests could pass vacuously.
#[test]
fn the_snapshot_helper_sees_a_file_that_appears() {
    let dir = TempDir::new().unwrap();
    let before = snapshot(dir.path());
    let nested = dir.path().join("profiles");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join("a.yaml"), "x").unwrap();
    let after = snapshot(dir.path());
    assert_ne!(before, after);
    assert!(after.contains_key("profiles/a.yaml"));
    assert_eq!(after["profiles/a.yaml"], "x");
    assert_eq!(
        PathBuf::from("profiles/a.yaml").parent(),
        Some(StdPath::new("profiles"))
    );
}
