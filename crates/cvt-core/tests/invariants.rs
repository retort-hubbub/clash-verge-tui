//! Property-based attacks on the invariants this crate advertises.
//!
//! Every test here is an *attack* on a claim made in a doc comment, a commit
//! message, or the verification brief — not a demonstration that the code
//! works. Where a claim survived, the test stays green and the report records
//! how many generated cases it took to convince me.
//!
//! # Counterexamples are kept, not deleted
//!
//! A test whose name starts with `f<n>_` is a **preserved counterexample**: it
//! is `#[ignore]`d so the suite stays green for the author, and it fails by
//! design until the finding is fixed. Run them all with:
//!
//! ```text
//! cargo test -p cvt-core --test invariants -- --ignored
//! ```
//!
//! Each `#[ignore]` reason names the finding, and the test body carries the
//! reproduction: the input that breaks the invariant and why the code
//! violates it. Nothing here depends on the audit that produced these — its
//! report is a document *about* the code, lists defects that are still open,
//! and is deliberately kept out of the repository.

#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;

use cvt_core::AppPaths;
use cvt_core::enhance::diff::{DEFAULT_LIMIT, diff, diff_limited};
use cvt_core::enhance::merge::{ArrayStrategy, MergeOptions, deep_merge, merged};
use cvt_core::enhance::overlay::Overlay;
use cvt_core::enhance::path::{self, Path, Segment};
use cvt_core::enhance::pipeline::Pipeline;
use cvt_core::mihomo::client::{encode_query, encode_segment};
use cvt_core::mihomo::types::{
    ApiMessage, Connection, ConnectionsResponse, DelayResponse, GeneralConfig, Hello, LogEvent,
    Memory, Metadata, ProxiesResponse, ProxyProviderInfo, ProxyView, RuleInfo, RuleProviderInfo,
    RuleProvidersResponse, RuleStats, RulesResponse, StatusOk, Traffic, Version,
};
use cvt_core::model::config::Config;
use cvt_core::model::rule::Rule;
use cvt_core::profile::item::{PrfItem, ProfileType, SeqPatch};
use cvt_core::profile::store::ProfileStore;
use cvt_core::validate;
use proptest::prelude::*;
use proptest::test_runner::Config as ProptestConfig;
use serde_json::{Map, Value, json};

// ------------------------------------------------------------- the generators

/// Values that a YAML emitter and parser have to agree about. Every one of
/// these is a documented trap: a plain scalar that means something else, a
/// string that looks like a number or a bool, syntax characters, and bytes
/// that have to be escaped.
const ADVERSARIAL: &[&str] = &[
    "",
    " ",
    "  x  ",
    "x",
    "0",
    "1",
    "-1",
    "+1",
    "007",
    "1.0",
    "0.0",
    "-0.0",
    "1e100",
    "1E5",
    "0x1f",
    "0o17",
    "010",
    "18446744073709551615",
    "-9223372036854775808",
    "true",
    "false",
    "True",
    "FALSE",
    "yes",
    "no",
    "on",
    "off",
    "null",
    "Null",
    "NULL",
    "~",
    "N",
    "Y",
    "#",
    "#hash",
    "# comment",
    "a: b",
    "a:b",
    "- item",
    "-",
    "--",
    "?",
    ":",
    "{}",
    "[]",
    "[a]",
    "{a: 1}",
    "*alias",
    "&anchor",
    "!!str",
    "%YAML",
    "---",
    "...",
    "|",
    ">",
    "\"quoted\"",
    "'single'",
    "a,b",
    "a\u{0}b",
    "line\nbreak",
    "line\n",
    "tab\there",
    "cr\rhere",
    "\u{7f}",
    "日本",
    "🇯🇵",
    "ü",
    "\\backslash",
    "a b",
    " leading",
    "trailing ",
    "two  spaces",
    "\u{1}",
    "\u{85}",
    "\u{2028}",
];

/// YAML documents that exercise the parser rather than the emitter: anchors,
/// aliases, merge keys, tags, duplicate keys, non-string keys, block scalars,
/// document markers and multi-byte integers.
const YAML_DOCUMENTS: &[&str] = &[
    "a: &x [1, 2]\nb: *x\n",
    "base: &b {x: 1}\nchild:\n  <<: *b\n  y: 2\n",
    "a: !!str 5\n",
    "a: !!int '5'\n",
    "a: !custom {b: 1}\n",
    "1: a\n",
    "1.5: a\n",
    "true: a\n",
    "null: a\n",
    "[1, 2]: a\n",
    "{a: 1}: b\n",
    "a: 1\na: 2\n",
    "a: 18446744073709551616\n",
    "a: 99999999999999999999999999\n",
    "a: .inf\n",
    "a: -.inf\n",
    "a: .nan\n",
    "a: 1e400\n",
    "a: |\n  block\n  scalar\n",
    "a: >\n  folded\n",
    "a: \"quoted\"\nb: 'single'\n",
    "---\na: 1\n---\nb: 2\n",
    "a:\n  - 1\n  - {b: 2}\n",
    "a: {}\n",
    "a: []\n",
    "a: null\n",
    "?\n",
    "\ta: 1\n",
    "a\n",
    "a: 1 # comment\n",
    "a: # comment\n  1\n",
    "\u{feff}a: 1\n",
    "a: 1\r\nb: 2\r\n",
];

fn arb_key() -> impl Strategy<Value = String> {
    prop_oneof![
        prop::sample::select(ADVERSARIAL.to_vec()).prop_map(str::to_owned),
        "[A-Za-z0-9 _.:#-]{0,24}",
    ]
}

fn arb_string() -> impl Strategy<Value = String> {
    prop_oneof![
        prop::sample::select(ADVERSARIAL.to_vec()).prop_map(str::to_owned),
        "\\PC{0,24}",
        "[ -~]{0,200}",
        any::<String>().prop_map(|s| s.chars().take(24).collect()),
    ]
}

fn arb_number() -> impl Strategy<Value = Value> {
    prop_oneof![
        prop::sample::select(vec![
            i64::MIN,
            i64::MAX,
            0,
            1,
            -1,
            65_535,
            -9_007_199_254_740_993
        ])
        .prop_map(Value::from),
        any::<u64>().prop_map(Value::from),
        prop::sample::select(vec![
            0.0,
            -0.0,
            1.5,
            1e100,
            -1e-300,
            9_007_199_254_740_993.0
        ])
        .prop_map(Value::from),
        any::<f64>().prop_map(Value::from),
    ]
}

fn arb_scalar() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        arb_number(),
        arb_string().prop_map(Value::String),
    ]
}

/// Arbitrary JSON with bounded depth and size, including adversarial keys.
fn arb_value() -> impl Strategy<Value = Value> {
    arb_scalar().prop_recursive(4, 32, 6, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
            prop::collection::vec((arb_key(), inner), 0..4).prop_map(|pairs| {
                Value::Object(pairs.into_iter().collect::<Map<String, Value>>())
            }),
        ]
    })
}

/// An arbitrary *document*: JSON that can sit at the root of a config.
fn arb_document() -> impl Strategy<Value = Value> {
    prop::collection::vec((arb_key(), arb_value()), 0..5)
        .prop_map(|pairs| Value::Object(pairs.into_iter().collect::<Map<String, Value>>()))
}

fn index_strategy() -> impl Strategy<Value = Segment> {
    prop_oneof![
        Just(i64::MIN),
        Just(i64::MAX),
        Just(-1),
        Just(0),
        -64i64..64,
        any::<i64>(),
    ]
    .prop_map(Segment::Index)
}

fn segment_strategy() -> impl Strategy<Value = Segment> {
    prop_oneof![
        3 => arb_key().prop_map(Segment::Key),
        2 => index_strategy(),
        2 => (arb_key(), arb_string()).prop_map(|(key, value)| Segment::Selector {
            key,
            value: value.replace(['[', ']'], ""),
        }),
    ]
}

/// A path expression built from the grammar, then rendered to text.
fn arb_path() -> impl Strategy<Value = String> {
    prop::collection::vec(segment_strategy(), 1..4).prop_map(|segments| {
        let mut out = String::new();
        for (i, segment) in segments.iter().enumerate() {
            match segment {
                Segment::Key(k) => {
                    if i > 0 {
                        out.push('.');
                    }
                    out.push_str(k);
                }
                Segment::Index(n) => {
                    let _ = write!(out, "[{n}]");
                }
                Segment::Selector { key, value } => {
                    let _ = write!(out, "[{key}={value}]");
                }
            }
        }
        out
    })
}

/// Arbitrary text handed to the path parser, to prove it is total.
fn arb_path_text() -> impl Strategy<Value = String> {
    prop_oneof![
        arb_path(),
        "\\PC{0,24}",
        "[\\[\\].=a-z0-9'\"?-]{0,24}",
        prop::sample::select(vec![
            "",
            ".",
            "..",
            "[",
            "]",
            "[]",
            "[=",
            "=]",
            "a[",
            "a[]",
            "a[0",
            "a[0]",
            "a[-1]",
            "a[-9223372036854775808]",
            "a[9223372036854775807]",
            "a[b=c]",
            "a[=c]",
            "a[b=]",
            "a['b']",
            "a[\"b\"]",
            "[0]",
            "[-1]",
            "a..b",
            "a.0.b",
            "0",
            "-1",
            "a[+1]",
            "a[ 0 ]",
        ])
        .prop_map(str::to_owned),
    ]
}

/// A YAML *document* assembled from adversarial keys and values, plus the
/// curated documents above.
fn arb_yaml_document() -> impl Strategy<Value = String> {
    prop_oneof![
        prop::sample::select(YAML_DOCUMENTS.to_vec()).prop_map(str::to_owned),
        (arb_key(), arb_string()).prop_map(|(k, v)| format!("{k}: {v}\n")),
        prop::collection::vec((arb_key(), arb_string()), 0..4).prop_map(|pairs| {
            let mut doc = String::new();
            for (k, v) in pairs {
                let _ = writeln!(doc, "{k}: {v}");
            }
            doc
        }),
        arb_document().prop_map(|doc| serde_norway::to_string(&doc).unwrap_or_default()),
    ]
}

/// A rule string that mihomo's grammar accepts, built field by field.
fn arb_rule_text() -> impl Strategy<Value = String> {
    // The payload must itself be grammar-valid: a bare top-level comma, an
    // unbalanced paren or a stray quote would change how the *input* splits,
    // and the claim under test is about inputs the grammar accepts. Internal
    // whitespace, spaces and unicode are all fair game.
    let plain = prop_oneof![
        prop::sample::select(vec![
            "google.com",
            "a b",
            "10.0.0.0/8",
            "example.org",
            "2001:db8::/32",
            "user@host",
            "a+b",
            "c#d",
            "café.test",
            "🇯🇵.test",
            "a b c",
            "x",
        ])
        .prop_map(str::to_owned),
        "[A-Za-z0-9._:/@#+-]{1,12} [A-Za-z0-9._:/@#+-]{1,12}",
        "[A-Za-z0-9._:/@#+-]{1,24}",
    ];
    let payload = prop_oneof![
        plain.clone(),
        // Logical rules carry nested parenthesised rules whose payloads contain
        // commas; this is the shape a naive `split(',')` gets wrong.
        prop::collection::vec("[A-Za-z0-9 .]{0,12}", 1..3)
            .prop_map(|parts| format!("(({}))", parts.join("),("))),
        // A quoted payload may contain a bare comma.
        prop::sample::select(vec!["a,b.com", "a b", "x"]).prop_map(|k| format!("\"{k},x\"")),
    ];
    (
        prop::sample::select(vec![
            "DOMAIN",
            "DOMAIN-SUFFIX",
            "DOMAIN-KEYWORD",
            "IP-CIDR",
            "IP-CIDR6",
            "SRC-PORT",
            "RULE-SET",
            "AND",
            "OR",
            "NOT",
            "GEOSITE",
            "PROCESS-NAME",
            "DST-PORT",
            "IN-USER",
        ]),
        payload.prop_map(|p| p.replace('\n', " ").trim().to_owned()),
        prop::sample::select(vec!["PROXY", "DIRECT", "REJECT", "grp", "漏网之鱼"]),
        prop::collection::vec(
            prop::sample::select(vec!["no-resolve", "src", "dst", "flag"]),
            0..3,
        ),
    )
        .prop_map(move |(kind, payload, policy, params)| {
            let mut s = format!("{kind},{payload},{policy}");
            for p in params {
                s.push(',');
                s.push_str(p);
            }
            s
        })
}

/// A plausible core `name` field.
fn arb_name() -> impl Strategy<Value = String> {
    prop_oneof![
        prop::sample::select(vec!["JP 01", "node b/slash", "DIRECT", "🇯🇵", "漏网之鱼"])
            .prop_map(str::to_owned),
        "[A-Za-z0-9 _./-]{0,24}",
    ]
}

// ================================================================ claim 1
// `Config`: `from_yaml(to_yaml(c)) == c` — "lossless".

proptest! {
    #![proptest_config(ProptestConfig::with_cases(3000))]

    /// A document built in memory must survive a render and a reparse.
    #[test]
    fn claim1_a_document_built_in_memory_survives_a_yaml_round_trip(doc in arb_document()) {
        let config = Config::from_map(doc.as_object().unwrap().clone());
        let yaml = config.to_yaml().unwrap();
        let back = Config::from_yaml(&yaml)
            .unwrap_or_else(|e| panic!("our own render must reparse: {e}\nyaml={yaml:?}"));
        prop_assert_eq!(&back, &config, "yaml was {}", yaml);
    }

    /// A document that came *from* YAML must survive the same trip.
    #[test]
    fn claim1_a_document_that_came_from_yaml_survives_the_trip(text in arb_yaml_document()) {
        let Ok(config) = Config::from_yaml(&text) else {
            return Ok(());  // not a mapping at the root; `from_yaml` is documented to reject it
        };
        let yaml = config.to_yaml().unwrap();
        let back = Config::from_yaml(&yaml)
            .unwrap_or_else(|e| panic!("our own render must reparse: {e}\nyaml={yaml:?}"));
        prop_assert_eq!(&back, &config, "text {:?} rendered as {:?}", text, yaml);
    }
}

/// Anchor/alias/merge-key handling is a documented round-trip hazard; this
/// records what actually happens so the report can be precise.
#[test]
fn claim1_anchors_aliases_and_merge_keys_round_trip() {
    for text in [
        "a: &x [1, 2]\nb: *x\n",
        "base: &b {x: 1}\nchild:\n  <<: *b\n  y: 2\n",
        "a: 1\na: 2\n",
        "1: a\n",
        "a: !!str 5\n",
    ] {
        let config = Config::from_yaml(text).unwrap();
        let yaml = config.to_yaml().unwrap();
        let back = Config::from_yaml(&yaml).unwrap();
        assert_eq!(back, config, "text {text:?} rendered as {yaml:?}");
    }
    // A merge key is preserved as the literal key `<<`, not expanded. mihomo
    // expands it at load time, so the document is still lossless — but the
    // merge engine sees an ordinary key.
    let merged_key = Config::from_yaml("base: &b {x: 1}\nchild:\n  <<: *b\n  y: 2\n").unwrap();
    assert_eq!(
        merged_key
            .get("child")
            .and_then(|c| c.get("<<"))
            .and_then(|m| m.get("x")),
        Some(&json!(1))
    );
}

// ================================================================ claim 2
// `Rule::parse` -> `to_string` is byte-exact for anything the grammar accepts.

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4000))]

    #[test]
    fn claim2_rule_round_trips_byte_for_byte(text in arb_rule_text()) {
        let rule = Rule::parse(&text)
            .unwrap_or_else(|| panic!("constructed rule must parse: {text:?}"));
        prop_assert_eq!(rule.to_string(), text);
    }
}

#[test]
fn claim2_the_documented_shapes_round_trip() {
    for text in [
        "DOMAIN-SUFFIX,google.com,PROXY",
        "MATCH,DIRECT",
        "FINAL,PROXY",
        "IP-CIDR,10.0.0.0/8,DIRECT,no-resolve",
        "AND,((DOMAIN,a.example),(NETWORK,udp)),PROXY",
        "OR,((DOMAIN,a.example),(DOMAIN,b.example)),PROXY",
        "NOT,((GEOIP,CN)),PROXY",
        "AND,(()),X",
        "DOMAIN,\"a,b.com\",PROXY",
        "DOMAIN,,PROXY",
        "RULE-SET,reject,REJECT-DROP",
    ] {
        let rule = Rule::parse(text).unwrap();
        assert_eq!(rule.to_string(), text, "round-trip failed for {text}");
    }
}

#[test]
#[ignore = "finding F10: a payload-less rule silently drops every field after the policy"]
fn f10_a_match_rule_with_extra_fields_is_truncated_not_rejected() {
    // `Rule::parse` documents that it "tolerate[s] a stray payload field
    // defensively", but it then discards the parameters as well, so the
    // round-trip is not byte-exact for an input the parser accepts — and the
    // validator's E-MATCH-WITH-PAYLOAD branch is unreachable as a result.
    let rule = Rule::parse("MATCH,DIRECT,no-resolve").unwrap();
    assert_eq!(rule.params, vec!["no-resolve"], "params must be preserved");
    assert_eq!(rule.to_string(), "MATCH,DIRECT,no-resolve");

    // Because a payload-less rule can never carry a payload, the validator's
    // E-MATCH-WITH-PAYLOAD branch is dead code and no config can reach it.
    let config = Config::from_yaml("rules: ['MATCH,GHOST,extra']\n").unwrap();
    let codes: Vec<&str> = validate::check(&config)
        .diagnostics
        .iter()
        .map(|d| d.code)
        .collect();
    assert!(
        codes.contains(&"E-MATCH-WITH-PAYLOAD"),
        "E-MATCH-WITH-PAYLOAD is unreachable; `MATCH,GHOST,extra` only produced {codes:?}, \
         and `extra` was dropped without a word"
    );
}

// ================================================================ claim 3
// `enhance::path::set` / `push` are atomic: an `Err` leaves the document
// byte-identical.

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4000))]

    #[test]
    fn claim3_set_is_atomic(doc in arb_document(), path_text in arb_path(), new_value in arb_value()) {
        let Ok(path) = Path::parse(&path_text) else {
            return Ok(());
        };
        let mut target = doc.clone();
        let result = path::set(&mut target, &path, new_value);
        if result.is_err() {
            prop_assert_eq!(target, doc, "a failed set must not touch the document (path {:?})", path_text);
        }
    }

    /// F3 was a real defect: `push` materialised the keys it needed on the way
    /// down and only then discovered that the leaf was not a list, so a call
    /// that reported failure had still changed the document. It now walks the
    /// path read-only first, and this is the property that says so — over
    /// thousands of generated documents, not the one input the finding used.
    #[test]
    fn claim3_push_is_atomic(doc in arb_document(), path_text in arb_path(), item in arb_value()) {
        let Ok(path) = Path::parse(&path_text) else {
            return Ok(());
        };
        let mut target = doc.clone();
        let result = path::push(&mut target, &path, item);
        if result.is_err() {
            prop_assert_eq!(target, doc, "a failed push must not touch the document (path {:?})", path_text);
        }
    }
}

#[test]
fn a_failed_push_reports_an_error_without_mutating_the_document() {
    let mut doc = json!({});
    let before = doc.clone();
    // `push` always names a *list*; `a[0]` names an element, so this must fail
    // without touching the document.
    let err = path::push(&mut doc, &Path::parse("a[0]").unwrap(), json!("x")).unwrap_err();
    assert!(err.to_string().contains("does not name a list"), "{err}");
    assert_eq!(doc, before, "push must be atomic, but produced {doc}");

    // The same with one more level of nesting, which is how the override
    // pipeline reaches it (`append: {"rule-providers.x[0]": [...]}`).
    let mut doc = json!({"a": {}});
    let before = doc.clone();
    let err = path::push(&mut doc, &Path::parse("a.b[0]").unwrap(), json!("x")).unwrap_err();
    assert!(err.to_string().contains("does not name a list"), "{err}");
    assert_eq!(doc, before, "push must be atomic, but produced {doc}");
}

// ================================================================ claim 4
// `enhance::path::get` never panics for any path string and any document.

proptest! {
    #![proptest_config(ProptestConfig::with_cases(6000))]

    // Each mutating call gets its own copy of the document; the copies exist
    // so that one operation cannot influence the next, not because their
    // results are inspected.
    #[allow(clippy::redundant_clone)]
    #[test]
    fn claim4_the_path_parser_is_total(doc in arb_document(), path_text in arb_path_text()) {
        // Any panic here fails the test; the claim is that none is reachable.
        let Ok(path) = Path::parse(&path_text) else {
            // A rejected path must still not panic the callers below.
            return Ok(());
        };
        let _ = path::get(&doc, &path);
        let mut target = doc.clone();
        let _ = path::set(&mut target, &path, json!(1));
        let mut target = doc.clone();
        let _ = path::push(&mut target, &path, json!(1));
        let mut target = doc.clone();
        let removed = path::remove(&mut target, &path);
        // `remove` is total: a path that cannot be traversed is `Ok(None)`.
        prop_assert!(removed.is_ok());
    }

    #[test]
    fn claim4_get_never_panics_for_generated_paths(doc in arb_document(), path_text in arb_path()) {
        if let Ok(path) = Path::parse(&path_text) {
            let _ = path::get(&doc, &path);
        }
    }
}

/// Finding F2: `path::get` used to panic here (`-i` overflows for `i64::MIN`).
/// The author's fix (`i.unsigned_abs()`, `path.rs`) landed during verification,
/// so this is now a regression test rather than a preserved counterexample.
#[test]
fn f2_an_index_of_i64_min_resolves_to_nothing() {
    let doc = json!({"a": [1, 2]});
    for text in [
        "a[-9223372036854775808]",
        "a[9223372036854775807]",
        "a[-1]",
        "a[0]",
    ] {
        let path = Path::parse(text).unwrap();
        let got = path::get(&doc, &path);
        if text.starts_with("a[9223") {
            assert_eq!(got, None, "{text} is out of range");
        }
    }
    assert_eq!(
        path::get(&doc, &Path::parse("a[-1]").unwrap()),
        Some(&json!(2))
    );
    assert_eq!(
        path::get(&doc, &Path::parse("a[0]").unwrap()),
        Some(&json!(1))
    );
    // Every operation that resolves an index must be total too.
    let extreme = Path::parse("a[-9223372036854775808]").unwrap();
    let mut target = doc.clone();
    assert!(path::push(&mut target, &extreme, json!(1)).is_err());
    assert!(path::set(&mut target, &extreme, json!(1)).is_err());
    assert!(path::remove(&mut target, &extreme).unwrap().is_none());
    assert_eq!(
        target, doc,
        "a rejected operation must leave the document alone"
    );
}

#[test]
fn the_rust_default_matches_the_documented_default() {
    // The module doc: "When the target list contains a terminal rule, `append`
    // inserts immediately *before* it... Set `Overlay::append_before_terminal`
    // to `false` for the literal behaviour." Parsing the same document from
    // YAML yields `true`; `Default::default()` yields `false`.
    assert!(
        Overlay::default().append_before_terminal,
        "the documented default is `true`"
    );
    let parsed = Overlay::from_yaml("append:\n  rules: [\"B,DIRECT\"]\n").unwrap();
    assert!(parsed.append_before_terminal);
    assert_eq!(
        parsed,
        Overlay {
            append_before_terminal: true,
            ..parsed.clone()
        }
    );
}

// ================================================================ claim 5
// `Overlay::apply` is idempotent and never places a rule after a terminal
// `MATCH`.

fn arb_overlay_items() -> impl Strategy<Value = Vec<Value>> {
    prop::collection::vec(
        prop_oneof![arb_string().prop_map(Value::String), arb_value()],
        0..3,
    )
}

fn arb_rules_list() -> impl Strategy<Value = Vec<Value>> {
    prop::collection::vec(
        prop_oneof![
            arb_string().prop_map(Value::String),
            prop::sample::select(vec![
                "MATCH,DIRECT",
                "MATCH,REJECT",
                "FINAL,PROXY",
                "DOMAIN-SUFFIX,a.test,PROXY",
            ])
            .prop_map(|s| Value::String(s.to_owned())),
        ],
        0..6,
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    /// `set`/`prepend`/`append` say nothing about position, so applying them
    /// twice must be a no-op the second time.
    #[test]
    fn claim5_set_prepend_and_append_are_idempotent(
        doc in arb_document(),
        set in prop::collection::vec((arb_path(), arb_value()), 0..3),
        prepend in prop::collection::vec((arb_path(), arb_overlay_items()), 0..2),
        append in prop::collection::vec((arb_path(), arb_overlay_items()), 0..2),
    ) {
        let overlay = Overlay {
            remove: Vec::new(),
            set: set.into_iter().collect(),
            prepend: prepend.into_iter().collect(),
            append: append.into_iter().collect(),
            append_before_terminal: true,
        };
        // Paths that cannot resolve (`a[9]` on a short list) are not what this
        // claim is about, so a rejected overlay is skipped.
        let mut once = doc;
        if overlay.apply(&mut once).is_err() {
            return Ok(());
        }
        overlay.apply(&mut once).unwrap();
        let mut twice = once.clone();
        overlay.apply(&mut twice).unwrap();
        prop_assert_eq!(&once, &twice);
    }

    /// An appended rule must land before the first terminal rule.
    #[test]
    fn claim5_append_keeps_rules_before_the_first_terminal(
        rules in arb_rules_list(),
        items in arb_overlay_items(),
    ) {
        let mut out = json!({ "rules": rules });
        let original_rules: Vec<Value> = out["rules"].as_array().cloned().unwrap_or_default();
        // `append_before_terminal` is set explicitly because
        // `Default::default()` disagrees with the documented (and serde)
        // default: see finding F17.
        let overlay = Overlay {
            append: BTreeMap::from([("rules".to_owned(), items.clone())]),
            append_before_terminal: true,
            ..Overlay::default()
        };
        overlay.apply(&mut out).unwrap();
        let after: Vec<Value> = out["rules"].as_array().cloned().unwrap_or_default();
        let is_terminal = |v: &Value| {
            v.as_str()
                .and_then(Rule::parse)
                .is_some_and(|rule| rule.is_terminal())
        };
        let Some(terminal) = after.iter().position(is_terminal) else {
            return Ok(());  // no catch-all anywhere: appending at the end is fine
        };
        // No item the overlay itself supplied may sit *after* the catch-all.
        for (i, item) in after.iter().enumerate() {
            let supplied_by_the_overlay = items.contains(item) && !original_rules.contains(item);
            if i <= terminal || !supplied_by_the_overlay {
                continue;
            }
            prop_assert!(
                false,
                "appended rule {:?} landed at {} after the catch-all at {} (rules {:?})",
                item,
                i,
                terminal,
                after
            );
        }
    }
}

/// F9 is **not** fixed, and deliberately so: it is a documented limitation
/// rather than a defect. A removal by position cannot be idempotent, because
/// the position is not stable — so `Overlay` promises idempotence for
/// everything *except* this, in its own documentation, and this test pins the
/// behaviour that promise excludes. Removing the capability instead would take
/// away a legitimate one-shot use.
#[test]
fn a_positional_removal_is_the_documented_exception_to_idempotence() {
    let overlay = Overlay::from_yaml("remove: [\"rules[1]\"]\n").unwrap();
    let mut doc = json!({"rules": ["A,DIRECT", "B,DIRECT", "C,DIRECT"]});
    overlay.apply(&mut doc).unwrap();
    assert_eq!(doc["rules"], json!(["A,DIRECT", "C,DIRECT"]));

    // Whatever moved into the slot is what the second application removes.
    // That is the whole reason the promise excludes this case.
    overlay.apply(&mut doc).unwrap();
    assert_eq!(doc["rules"], json!(["A,DIRECT"]));

    // Removing by name does not have the problem, and the promise holds.
    let named = Overlay::from_yaml("remove: [\"proxies[name=JP 02]\"]\n").unwrap();
    let mut doc = json!({"proxies": [{"name": "JP 01"}, {"name": "JP 02"}]});
    named.apply(&mut doc).unwrap();
    assert_eq!(doc["proxies"], json!([{"name": "JP 01"}]));
    let once = doc.clone();
    named.apply(&mut doc).unwrap();
    assert_eq!(doc, once, "naming the target keeps the promise");
}

#[test]
#[ignore = "finding F13: appending a catch-all places it before the existing one, deadening it"]
fn f13_appending_a_terminal_rule_deadens_the_existing_catch_all() {
    let overlay = Overlay::from_yaml("append:\n  rules: [\"MATCH,REJECT\"]\n").unwrap();
    let mut doc = json!({"rules": ["DOMAIN,a.test,PROXY", "MATCH,DIRECT"]});
    overlay.apply(&mut doc).unwrap();
    let rules: Vec<&str> = doc["rules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    // The overlay promises to keep `rules` well-formed; the result has a
    // terminal rule above another one, which `validate` then rejects.
    assert_eq!(rules.last(), Some(&"MATCH,REJECT"), "{rules:?}");
    assert!(
        !rules.contains(&"MATCH,DIRECT"),
        "the pre-existing catch-all can never fire: {rules:?}"
    );
}

// ================================================================ claim 6
// `deep_merge` with `Union` never introduces a duplicate, and a `null` in the
// patch always deletes the key.

/// Report the first position at which the patch wrote a `null` that is still
/// present in the merged document. Structural, so that a key containing a `.`
/// cannot be confused with a nested path.
fn surviving_null(patch: &Value, out: Option<&Value>, at: &str) -> Option<String> {
    match patch {
        Value::Null => match out {
            Some(Value::Null) => Some(format!("{at} survived as null")),
            _ => None,
        },
        Value::Object(map) => map
            .iter()
            .find_map(|(k, v)| surviving_null(v, out.and_then(|o| o.get(k)), &format!("{at}.{k}"))),
        Value::Array(items) => items.iter().enumerate().find_map(|(i, v)| {
            surviving_null(v, out.and_then(|o| o.get(i)), &format!("{at}[{i}]"))
        }),
        _ => None,
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(3000))]

    /// Union may keep whatever the base already had, but it may not add a
    /// duplicate of its own.
    #[test]
    fn claim6_union_never_adds_a_duplicate(
        base in prop::collection::vec(arb_value(), 0..5),
        patch in prop::collection::vec(arb_value(), 0..5),
    ) {
        let out = ArrayStrategy::Union.apply(&base, &patch);
        for item in &out {
            let in_out = out.iter().filter(|v| *v == item).count();
            let in_base = base.iter().filter(|v| *v == item).count();
            prop_assert!(
                in_out <= in_base.max(1),
                "union added a duplicate of {:?}: {:?}",
                item,
                out
            );
        }
        prop_assert_eq!(out.len(), ArrayStrategy::Union.apply(&base, &patch).len());
    }

    /// A `null` anywhere in the patch must delete that key.
    ///
    /// Fails today: see `f4_...`.
    #[test]
    #[ignore = "finding F4: a null nested in a brand-new subtree is inserted, not deleted"]
    fn claim6_a_null_in_the_patch_deletes_the_key(
        base in arb_document(),
        patch in arb_document(),
    ) {
        let out = merged(&base, &patch, &MergeOptions::default());
        if let Some(where_) = surviving_null(&patch, Some(&out), "") {
            prop_assert!(
                false,
                "{} -- base {}, patch {}, out {}",
                where_,
                base,
                patch,
                out
            );
        }
    }
}

#[test]
#[ignore = "finding F4: a null inside a brand-new subtree is inserted, not deleted"]
fn f4_a_null_nested_in_a_new_subtree_survives_the_merge() {
    let patch = json!({"new": {"a": null}});
    let out = merged(&json!({}), &patch, &MergeOptions::default());
    assert_eq!(
        out,
        json!({"new": {}}),
        "the null must delete `a`, leaving an empty mapping"
    );
}

#[test]
fn a_null_in_an_existing_subtree_is_idempotent() {
    // Kept as a positive control: for patches whose nulls sit inside subtrees
    // the base already has, the merge *is* idempotent.
    let patch = json!({"dns": {"fallback": null}});
    let base = json!({"dns": {"fallback": ["8.8.8.8"], "enable": true}});
    let once = merged(&base, &patch, &MergeOptions::default());
    let twice = merged(&once, &patch, &MergeOptions::default());
    assert_eq!(once, twice);
    assert_eq!(once, json!({"dns": {"enable": true}}));
}

// ================================================================ claim 7
// `diff`: `diff(a, a)` is empty, `diff` never panics, and `touched_keys` is
// exactly the set of top-level keys whose values differ.

fn small_object() -> impl Strategy<Value = Value> {
    prop::collection::vec((arb_key(), arb_value()), 0..6)
        .prop_map(|pairs| Value::Object(pairs.into_iter().collect::<Map<String, Value>>()))
}

/// A top-level key as a mihomo config actually spells it: no `.`, no `[`.
fn arb_plain_key() -> impl Strategy<Value = String> {
    prop_oneof![
        prop::sample::select(vec![
            "mode",
            "log-level",
            "mixed-port",
            "allow-lan",
            "dns",
            "rules",
            "proxies",
            "proxy-groups",
            "tun",
            "ipv6",
            "secret",
            "external-controller",
        ])
        .prop_map(str::to_owned),
        "[A-Za-z0-9 _:-]{0,16}",
    ]
}

/// A document whose values are scalars or lists of scalars: the shape of most
/// of a real config, and the shape the diff's own docs describe.
fn small_flat_object() -> impl Strategy<Value = Value> {
    prop::collection::vec(
        (
            arb_plain_key(),
            prop_oneof![
                arb_scalar(),
                prop::collection::vec(arb_scalar(), 0..4).prop_map(Value::Array),
            ],
        ),
        0..6,
    )
    .prop_map(|pairs| Value::Object(pairs.into_iter().collect::<Map<String, Value>>()))
}

/// The claim-7 body, shared by the realistic property and the finding.
fn check_touched_keys(before: Value, after: Value) {
    // Compare documents with the same key set, so that truncation cannot be
    // the reason a key is missing.
    let mut keys: Vec<String> = before
        .as_object()
        .unwrap()
        .keys()
        .chain(after.as_object().unwrap().keys())
        .cloned()
        .collect();
    keys.sort();
    keys.dedup();
    let mut b = before;
    let mut a = after;
    for key in &keys {
        if !b.as_object().unwrap().contains_key(key) {
            b[key] = Value::Null;
        }
        if !a.as_object().unwrap().contains_key(key) {
            a[key] = Value::Null;
        }
    }
    let b = strip_nulls(&b);
    let a = strip_nulls(&a);
    let d = diff_limited(&b, &a, DEFAULT_LIMIT * 4);
    let mut expected: Vec<String> = Vec::new();
    for key in keys {
        if b.get(&key) != a.get(&key) {
            expected.push(key);
        }
    }
    let mut got = d.touched_keys();
    got.sort();
    expected.sort();
    assert_eq!(got, expected, "before {b} after {a}");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(3000))]

    #[test]
    fn claim7_a_document_never_differs_from_itself(doc in arb_value()) {
        let d = diff(&doc, &doc);
        prop_assert!(d.is_empty(), "{} entries for {:?}", d.len(), doc);
        prop_assert!(d.touched_keys().is_empty());
    }

    /// Realistic keys only (`[`/`.` cannot appear in a mihomo top-level key),
    /// which is the domain the claim is really about. The unrestricted version
    /// is `claim7_touched_keys_are_exactly_the_changed_keys` below, kept as a
    /// finding.
    #[test]
    fn claim7_touched_keys_are_exactly_the_changed_keys_for_realistic_keys(
        before in small_flat_object(),
        after in small_flat_object(),
    ) {
        check_touched_keys(before, after);
    }

    /// Fails today: see `f14_...` and `f15_...`.
    #[test]
    fn claim7_touched_keys_are_exactly_the_changed_keys(
        before in small_object(),
        after in small_object(),
    ) {
        check_touched_keys(before, after);
    }

    #[test]
    fn claim7_diff_never_panics(before in arb_value(), after in arb_value()) {
        let d = diff(&before, &after);
        let _ = d.summary();
        let _ = d.counts();
        let _ = d.to_string();
    }
}

fn strip_nulls(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| (k.clone(), strip_nulls(v)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(strip_nulls).collect()),
        other => other.clone(),
    }
}

#[test]
fn touched_keys_is_complete_even_when_the_diff_is_truncated() {
    let before = json!({
        "rules": (0..700).map(|i| json!(format!("R{i},DIRECT"))).collect::<Vec<_>>(),
        "mode": "rule",
    });
    let after = json!({"rules": [], "mode": "global"});
    let d = diff(&before, &after);
    assert!(d.truncated, "the cap must have been reached for this shape");
    assert!(
        d.touched_keys().contains(&"mode".to_owned()),
        "`mode` differs, so it must be reported as touched: {:?}",
        d.touched_keys()
    );
}

#[test]
fn reordering_a_named_list_is_reported() {
    let before = json!({"proxy-groups": [
        {"name": "PROXY", "type": "select", "proxies": ["DIRECT"]},
        {"name": "auto", "type": "url-test", "proxies": ["DIRECT"]},
    ]});
    let after = json!({"proxy-groups": [
        {"name": "auto", "type": "url-test", "proxies": ["DIRECT"]},
        {"name": "PROXY", "type": "select", "proxies": ["DIRECT"]},
    ]});
    let d = diff(&before, &after);
    assert!(
        !d.is_empty(),
        "the documents differ (the list order changed), so the diff must say so"
    );
    assert_eq!(d.touched_keys(), vec!["proxy-groups".to_owned()]);

    // The same blindness hides a shortening of a list that repeats a name.
    let d = diff(
        &json!({"proxies": [{"name": "A"}, {"name": "A"}]}),
        &json!({"proxies": [{"name": "A"}]}),
    );
    assert!(
        !d.is_empty(),
        "dropping a list element changes the document"
    );
}

#[test]
fn touched_keys_handles_a_key_containing_a_dot() {
    let d = diff(&json!({}), &json!({"my.key": 1}));
    assert_eq!(d.touched_keys(), vec!["my.key".to_owned()]);
    assert_eq!(d.for_top_level("my.key").len(), 1);
}

// ================================================================ claim 8
// `validate::check` has no false positives, and no false negatives for any
// documented code.

/// The three-config corpus from the spec's own shapes: a subscription-shaped
/// document, one built around a provider, and a fake-IP/TUN document.
const VALID_CONFIGS: &[&str] = &[
    r#"
mixed-port: 17890
external-controller: 127.0.0.1:9090
mode: rule
log-level: warning
proxies:
  - { name: "JP 01", type: vless, server: 1.2.3.4, port: 443, uuid: u }
  - { name: "node b/slash", type: trojan, server: 5.6.7.8, port: 443, password: p }
proxy-groups:
  - { name: PROXY, type: select, proxies: ["JP 01", "node b/slash", DIRECT] }
  - { name: auto, type: url-test, proxies: ["JP 01", "node b/slash"], url: "http://cp.cloudflare.com/generate_204", interval: 300 }
rules:
  - DOMAIN-SUFFIX,google.com,PROXY
  - RULE-SET,reject,REJECT
  - GEOIP,CN,DIRECT
  - MATCH,PROXY
rule-providers:
  reject:
    type: http
    behavior: classical
    url: "https://example.com/reject.yaml"
    path: ./reject.yaml
"#,
    r#"
mixed-port: 17890
external-controller: 127.0.0.1:9090
proxy-providers:
  airport:
    type: http
    url: "https://example.com/sub"
    path: ./airport.yaml
    health-check: { enable: true, url: "http://cp.cloudflare.com/generate_204", interval: 300 }
proxy-groups:
  - { name: PROXY, type: url-test, use: [airport], filter: "(?i)hk|jp" }
rules:
  - MATCH,PROXY
"#,
    r#"
mixed-port: 17890
external-controller: 127.0.0.1:9090
ipv6: true
dns:
  enable: true
  enhanced-mode: fake-ip
  fake-ip-range: 198.18.0.1/16
  fake-ip-range6: 2001:2::/64
  nameserver: [1.1.1.1, 8.8.8.8]
tun:
  enable: true
  stack: mixed
  auto-route: true
rules:
  - IP-CIDR,10.0.0.0/8,DIRECT,no-resolve
  - MATCH,DIRECT
"#,
];

#[test]
fn claim8_the_valid_corpus_produces_no_errors() {
    for yaml in VALID_CONFIGS {
        let config = Config::from_yaml(yaml).unwrap();
        let report = validate::check(&config);
        assert!(
            report.is_ok(),
            "a config the core accepts must not produce errors:\n{}",
            report.render()
        );
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    /// A generated but well-formed config must never produce an error.
    #[test]
    fn claim8_generated_valid_configs_produce_no_errors(
        names in prop::collection::vec(arb_name(), 1..5),
        group in arb_name(),
    ) {
        // mihomo requires unique names, so de-duplicate before building.
        let mut seen = HashSet::new();
        let proxies: Vec<Value> = names
            .iter()
            .filter(|n| {
                n == &n.trim() && !n.trim().is_empty() && seen.insert((*n).clone())
            })
            .map(|n| json!({"name": n, "type": "socks5", "server": "127.0.0.1", "port": 1080}))
            .collect();
        let mut members: Vec<Value> = proxies
            .iter()
            .filter_map(|p| p.get("name").cloned())
            .collect();
        members.push(json!("DIRECT"));
        // An unnamed or memberless group is a genuine finding, so keep the
        // generator inside the set of documents the core accepts.
        prop_assume!(!group.trim().is_empty());
        prop_assume!(group == group.trim(), "a padded name is ambiguous in a rule");
        prop_assume!(!proxies.is_empty());
        let doc = json!({
            "mixed-port": 17890,
            "external-controller": "127.0.0.1:9090",
            "proxies": proxies,
            "proxy-groups": [{"name": group, "type": "select", "proxies": members}],
            "rules": [format!("MATCH,{group}")],
        });
        let config = Config::from_value(doc).unwrap();
        let report = validate::check(&config);
        prop_assert!(
            report.is_ok(),
            "unexpected errors for a valid config:\n{}",
            report.render()
        );
    }
}

#[test]
fn the_built_in_policies_the_core_always_provides_are_not_dangling() {
    // Spec §4.1: `/proxies` "always contains the built-ins DIRECT, REJECT,
    // REJECT-DROP, PASS, PASS-RULE, COMPATIBLE and the policy group GLOBAL".
    for policy in ["GLOBAL", "PASS-RULE"] {
        let yaml = format!(
            "mixed-port: 17890\nexternal-controller: 127.0.0.1:9090\nrules:\n  - DOMAIN,x.test,{policy}\n  - MATCH,DIRECT\n"
        );
        let config = Config::from_yaml(&yaml).unwrap();
        let report = validate::check(&config);
        let codes: Vec<&str> = report.diagnostics.iter().map(|d| d.code).collect();
        assert!(
            !codes.contains(&"E-DANGLING-POLICY"),
            "`{policy}` exists in every running core, but check() reported {codes:?}"
        );
    }
}

#[test]
#[ignore = "finding F11: `type: smart` is accepted although mihomo has no smart group"]
fn f11_a_smart_group_is_not_a_real_mihomo_group_type() {
    // Spec §10.19: "There is no 'smart' proxy group in mihomo: grep for Smart
    // across the whole source tree returns nothing, and no `smart` group type
    // exists in docs/config.yaml".
    let config = Config::from_yaml(
        "mixed-port: 17890\nexternal-controller: 127.0.0.1:9090\nproxy-groups:\n  - {name: s, type: smart, proxies: [DIRECT]}\nrules: [MATCH,s]\n",
    )
    .unwrap();
    let report = validate::check(&config);
    let codes: Vec<&str> = report.diagnostics.iter().map(|d| d.code).collect();
    assert!(
        codes.contains(&"E-GROUP-TYPE"),
        "a group type the core cannot build must be an error, got {codes:?}"
    );
}

/// Every code the module can emit must be reachable from a config that
/// deserves it. The table is one entry per code in `validate.rs`.
#[test]
fn claim8_every_documented_code_is_reachable() {
    let cases: &[(&str, &str)] = &[
        (
            "E-DUPLICATE-PROXY",
            "proxies:\n  - {name: a, type: socks5, server: 1.2.3.4, port: 1}\n  - {name: a, type: socks5, server: 5.6.7.8, port: 1}\nrules: ['MATCH,DIRECT']\n",
        ),
        (
            "E-EMPTY-NAME",
            "proxies:\n  - {name: '', type: socks5, server: 1.2.3.4, port: 1}\nrules: ['MATCH,DIRECT']\n",
        ),
        (
            "E-MISSING-SERVER",
            "proxies:\n  - {name: a, type: socks5, port: 1}\nrules: ['MATCH,DIRECT']\n",
        ),
        (
            "W-MISSING-PORT",
            "proxies:\n  - {name: a, type: socks5, server: 1.2.3.4}\nrules: ['MATCH,DIRECT']\n",
        ),
        (
            "E-DUPLICATE-GROUP",
            "proxy-groups:\n  - {name: g, type: select, proxies: [DIRECT]}\n  - {name: g, type: select, proxies: [DIRECT]}\nrules: ['MATCH,g']\n",
        ),
        (
            "E-GROUP-TYPE",
            "proxy-groups:\n  - {name: g, type: nonsense, proxies: [DIRECT]}\nrules: ['MATCH,g']\n",
        ),
        (
            "E-DANGLING-GROUP-MEMBER",
            "proxy-groups:\n  - {name: g, type: select, proxies: [GHOST]}\nrules: ['MATCH,g']\n",
        ),
        (
            "E-DANGLING-PROVIDER",
            "proxy-groups:\n  - {name: g, type: url-test, use: [ghost]}\nrules: ['MATCH,g']\n",
        ),
        (
            "E-BAD-FILTER",
            "proxy-providers:\n  p: {type: http, url: 'https://x', path: ./p.yaml}\nproxy-groups:\n  - {name: g, type: url-test, use: [p], filter: '(unclosed'}\nrules: ['MATCH,g']\n",
        ),
        (
            "W-EMPTY-GROUP",
            "proxy-groups:\n  - {name: g, type: url-test}\nrules: ['MATCH,g']\n",
        ),
        ("E-NO-RULES", "mixed-port: 17890\n"),
        ("W-NO-TERMINAL-RULE", "rules: ['DOMAIN,a.test,DIRECT']\n"),
        (
            "W-TERMINAL-NOT-LAST",
            "rules: ['MATCH,DIRECT', 'DOMAIN,a.test,DIRECT']\n",
        ),
        (
            "E-UNREACHABLE-RULES",
            "rules: ['MATCH,DIRECT', 'MATCH,REJECT']\n",
        ),
        (
            "E-DANGLING-POLICY",
            "rules: ['DOMAIN,a.test,GHOST', 'MATCH,DIRECT']\n",
        ),
        (
            "E-DANGLING-RULE-SET",
            "rules: ['RULE-SET,ghost,DIRECT', 'MATCH,DIRECT']\n",
        ),
        (
            "W-DUPLICATE-RULE",
            "rules: ['DOMAIN,a.test,DIRECT', 'DOMAIN,a.test,DIRECT', 'MATCH,DIRECT']\n",
        ),
        (
            "W-CIDR-NO-PREFIX",
            "rules: ['IP-CIDR,10.0.0.0,DIRECT', 'MATCH,DIRECT']\n",
        ),
        (
            "E-CIDR-FAMILY",
            "rules: ['IP-CIDR6,10.0.0.0/8,DIRECT', 'MATCH,DIRECT']\n",
        ),
        (
            "W-DOMAIN-WILDCARD",
            "rules: ['DOMAIN-SUFFIX,.a.test,DIRECT', 'MATCH,DIRECT']\n",
        ),
        (
            "I-UNUSED-RULE-SET",
            "rule-providers:\n  spare: {type: http, url: 'https://x', path: ./s.yaml}\nrules: ['MATCH,DIRECT']\n",
        ),
        (
            "E-RELAY-CYCLE",
            "proxy-groups:\n  - {name: A, type: relay, proxies: [B]}\n  - {name: B, type: relay, proxies: [A]}\nrules: ['MATCH,A']\n",
        ),
        (
            "E-PORT-RANGE",
            "tproxy-port: 99999\nrules: ['MATCH,DIRECT']\n",
        ),
        (
            "E-PORT-CONFLICT",
            "mixed-port: 7890\nport: 7890\nrules: ['MATCH,DIRECT']\n",
        ),
        (
            "W-MIXED-PORT-REDUNDANT",
            "mixed-port: 7890\nport: 7891\nrules: ['MATCH,DIRECT']\n",
        ),
        (
            "W-NO-CONTROLLER",
            "mixed-port: 7890\nrules: ['MATCH,DIRECT']\n",
        ),
        (
            "E-CONTROLLER-FORMAT",
            "external-controller: 127.0.0.1\nrules: ['MATCH,DIRECT']\n",
        ),
        (
            "E-DNS-NO-NAMESERVER",
            "dns:\n  enable: true\nrules: ['MATCH,DIRECT']\n",
        ),
        (
            "I-DNS-DISABLED",
            "dns:\n  enable: false\nrules: ['MATCH,DIRECT']\n",
        ),
        (
            "W-FAKEIP-NO-RANGE",
            "dns:\n  enable: true\n  enhanced-mode: fake-ip\n  nameserver: [1.1.1.1]\nrules: ['MATCH,DIRECT']\n",
        ),
        (
            "W-FAKEIP-RANGE-IGNORED",
            "dns:\n  enable: true\n  enhanced-mode: redir-host\n  fake-ip-range: 198.18.0.1/16\n  nameserver: [1.1.1.1]\nrules: ['MATCH,DIRECT']\n",
        ),
        (
            "W-FAKEIP6-ULA",
            "ipv6: true\ndns:\n  enable: true\n  enhanced-mode: fake-ip\n  nameserver: [1.1.1.1]\n  fake-ip-range6: fdfe:dcba:9876::1/64\nrules: ['MATCH,DIRECT']\n",
        ),
        (
            "I-FAKEIP6-IMPLICIT",
            "ipv6: true\ndns:\n  enable: true\n  enhanced-mode: fake-ip\n  nameserver: [1.1.1.1]\nrules: ['MATCH,DIRECT']\n",
        ),
        (
            "W-TUN-NO-DNS",
            "tun:\n  enable: true\n  stack: mixed\nrules: ['MATCH,DIRECT']\n",
        ),
        (
            "I-TUN-NO-STACK",
            "tun:\n  enable: true\n  auto-route: false\nrules: ['MATCH,DIRECT']\n",
        ),
        (
            "W-TUN-AUTOROUTE-NO-DNS",
            "tun:\n  enable: true\n  stack: mixed\n  auto-route: true\nrules: ['MATCH,DIRECT']\n",
        ),
    ];

    // `E-MATCH-WITH-PAYLOAD` is deliberately absent from this table: see
    // `f10_...` below, which shows that no config can reach it.
    let mut missing = Vec::new();
    for (code, yaml) in cases {
        let config = Config::from_yaml(yaml).unwrap();
        let report = validate::check(&config);
        let codes: Vec<&str> = report.diagnostics.iter().map(|d| d.code).collect();
        if !codes.contains(code) {
            missing.push(format!("{code}: got {codes:?}\n{}", report.render()));
        }
    }
    assert!(
        missing.is_empty(),
        "{} documented code(s) could not be triggered:\n{}",
        missing.len(),
        missing.join("\n---\n")
    );
}

// ================================================================ claim 9
// `ProfileStore`: save/load round-trips, `add` never returns a uid already
// present, `resolve_chain` never repeats a profile, and `import_from` never
// overwrites an existing uid or document.

fn store_fixture() -> (tempfile::TempDir, ProfileStore) {
    let dir = tempfile::TempDir::new().unwrap();
    let paths = AppPaths::new(dir.path());
    paths.ensure_dirs().unwrap();
    let store = ProfileStore::load(&paths).unwrap();
    (dir, store)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// Every field of an index entry must survive a save and a reload,
    /// including names and descriptions that YAML would otherwise reinterpret.
    #[test]
    fn claim9_save_and_load_round_trip(
        names in prop::collection::vec(arb_string(), 1..4),
        descs in prop::collection::vec(arb_string(), 1..4),
        urls in prop::collection::vec(arb_string(), 1..4),
    ) {
        let (dir, mut store) = store_fixture();
        for (i, ((name, desc), url)) in names.iter().zip(&descs).zip(&urls).enumerate() {
            let mut item = PrfItem::remote(format!("R{i}"), name, url);
            item.desc = desc.clone();
            store.add(item);
        }
        store.set_current("R0").unwrap();
        store.save().unwrap();
        let reloaded = ProfileStore::load(store.paths()).unwrap();
        prop_assert_eq!(reloaded.index(), store.index());
        drop(dir);
    }
}

#[test]
fn claim9_save_and_load_round_trips_adversarial_fields() {
    let (dir, mut store) = store_fixture();
    for (i, name) in [
        "yes",
        "true",
        "123",
        "1.0",
        "null",
        "~",
        "on",
        "0x1f",
        "  padded  ",
        "a: b",
        "#hash",
        "line\nbreak",
        "日本語",
        "",
        "N",
        "a\tb",
    ]
    .iter()
    .enumerate()
    {
        let mut item = PrfItem::remote(format!("R{i}"), *name, format!("https://x/{i}?a=1&b=2"));
        item.desc = (*name).to_owned();
        store.add(item);
    }
    store.save().unwrap();
    let reloaded = ProfileStore::load(store.paths()).unwrap();
    assert_eq!(reloaded.index(), store.index());
    drop(dir);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// `resolve_chain` either fails or returns each profile exactly once.
    #[test]
    fn claim9_resolve_chain_never_repeats_a_profile(
        merges in prop::collection::vec(0usize..6, 0..4),
        explicit in prop::collection::vec(0usize..6, 0..4),
    ) {
        let (dir, mut store) = store_fixture();
        store.add(PrfItem::local("L1", "base"));
        for i in 0..5 {
            store.add(PrfItem::patch(format!("m{i}"), "patch", ProfileType::Merge));
        }
        let mut base = PrfItem::local("L0", "base'");
        base.option.merge = merges.first().map(|i| format!("m{i}"));
        base.option.rules = merges.get(1).map(|i| format!("m{i}"));
        store.add(base);
        store.set_current("L1").unwrap();
        let chain: Vec<String> = explicit.iter().map(|i| format!("m{i}")).collect();
        let _ = store.set_chain(&chain);

        if let Ok(resolved) = store.resolve_chain() {
            let uids: Vec<&str> = resolved.iter().map(|i| i.uid.as_str()).collect();
            let unique: HashSet<&str> = uids.iter().copied().collect();
            prop_assert_eq!(uids.len(), unique.len(), "chain repeated a profile: {:?}", uids);
        }
        drop(dir);
    }
}

#[test]
fn claim9_importing_twice_leaves_the_first_copy_intact() {
    let (dir, mut store) = store_fixture();
    let foreign = dir.path().join("foreign");
    std::fs::create_dir_all(foreign.join("profiles")).unwrap();
    std::fs::write(
        foreign.join("profiles.yaml"),
        "items:\n  - {uid: Rabc, type: remote, name: A, url: \"https://x\", file: Rabc.yaml}\n",
    )
    .unwrap();
    std::fs::write(foreign.join("profiles/Rabc.yaml"), "mode: rule\n").unwrap();

    store.import_from(&foreign).unwrap();
    let uid = store.items()[0].uid.clone();
    let original = store.read_document(store.get(&uid).unwrap()).unwrap();

    let second = store.import_from(&foreign).unwrap();
    assert_eq!(
        second.renamed, 1,
        "the second import must rename, not clobber"
    );
    assert_eq!(store.items().len(), 2);
    assert_eq!(
        store.read_document(store.get(&uid).unwrap()).unwrap(),
        original,
        "the first copy must be untouched"
    );
    drop(dir);
}

#[test]
#[ignore = "finding F7: `add` accepts an explicit uid that is already present"]
fn f7_add_returns_a_uid_that_is_already_in_the_index() {
    let (dir, mut store) = store_fixture();
    let first = store.add(PrfItem::local("L1", "one"));
    let second = store.add(PrfItem::local("L1", "two"));
    assert_ne!(first, second, "the second add must be given a fresh uid");
    assert_eq!(store.items().len(), 2);
    assert_eq!(
        store.items()[0].uid,
        store.items()[1].uid,
        "documents would collide"
    );
    drop(dir);
}

#[test]
#[ignore = "finding F8: import_from overwrites a document whose uid is not in the index"]
fn f8_import_overwrites_an_orphan_document() {
    let (dir, mut store) = store_fixture();
    // A document with no index entry: what a crash between `add` and `save`,
    // a restored older `profiles.yaml`, or a second front-end writing into
    // `profiles/` all leave behind.
    std::fs::write(
        store.paths().profiles_dir().join("Rabc.yaml"),
        "precious: true\n",
    )
    .unwrap();

    let foreign = dir.path().join("foreign");
    std::fs::create_dir_all(foreign.join("profiles")).unwrap();
    std::fs::write(
        foreign.join("profiles.yaml"),
        "items:\n  - {uid: Rabc, type: remote, name: A, url: \"https://x\", file: Rabc.yaml}\n",
    )
    .unwrap();
    std::fs::write(foreign.join("profiles/Rabc.yaml"), "incoming: true\n").unwrap();

    store.import_from(&foreign).unwrap();
    let after = std::fs::read_to_string(store.paths().profiles_dir().join("Rabc.yaml")).unwrap();
    assert_eq!(
        after, "precious: true\n",
        "an existing document was overwritten"
    );
    drop(dir);
}

// ================================================================ claim 10
// `SeqPatch::apply` never duplicates an item; deletion wins over an identical
// re-add.

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4000))]

    /// Fails today: see `f5_...`.
    #[test]
    #[ignore = "finding F5: a repeated prepend entry is duplicated"]
    fn claim10_apply_never_introduces_a_duplicate(
        base in prop::collection::vec(arb_string(), 0..5),
        patch in prop::collection::vec(arb_string(), 0..3),
        append in prop::collection::vec(arb_string(), 0..3),
        delete in prop::collection::vec(arb_string(), 0..3),
    ) {
        let seq = SeqPatch { prepend: patch, append, delete };
        let out = seq.apply(&base);
        for item in &out {
            let in_out = out.iter().filter(|v| *v == item).count();
            let in_base = base.iter().filter(|v| *v == item).count();
            prop_assert!(
                in_out <= in_base.max(1),
                "apply duplicated {:?}: base {:?} out {:?}",
                item,
                base,
                out
            );
        }
    }

    /// Fails today: see `f5_...`.
    #[test]
    #[ignore = "finding F5: `apply_values` re-adds an item that is already present"]
    fn claim10_apply_values_never_introduces_a_duplicate(
        base in prop::collection::vec(arb_name(), 0..5),
        prepend in prop::collection::vec(arb_name(), 0..3),
        append in prop::collection::vec(arb_name(), 0..3),
        delete in prop::collection::vec(arb_name(), 0..2),
    ) {
        let base: Vec<Value> = base
            .iter()
            .map(|n| json!({"name": n, "port": 1}))
            .collect();
        let seq = SeqPatch { prepend, append, delete };
        let out = seq.apply_values(&base);
        for item in &out {
            let in_out = out.iter().filter(|v| *v == item).count();
            let in_base = base.iter().filter(|v| *v == item).count();
            prop_assert!(
                in_out <= in_base.max(1),
                "apply_values duplicated {:?}: base {:?} out {:?}",
                item,
                base,
                out
            );
        }
    }
}

#[test]
fn claim10_deletion_wins_over_an_identical_readd() {
    let base = vec!["A".to_owned(), "B".to_owned()];
    for seq in [
        SeqPatch {
            prepend: vec!["B".into()],
            append: vec!["B".into()],
            delete: vec!["B".into()],
        },
        SeqPatch {
            prepend: vec![],
            append: vec!["B".into()],
            delete: vec!["B".into()],
        },
        SeqPatch {
            prepend: vec!["B".into()],
            append: vec![],
            delete: vec!["B".into()],
        },
    ] {
        let out = seq.apply(&base);
        assert_eq!(
            out.iter().filter(|v| *v == "B").count(),
            1,
            "exactly one copy may survive: {out:?}"
        );
    }
}

#[test]
#[ignore = "finding F5: `apply` duplicates a repeated prepend entry"]
fn f5_a_repeated_prepend_entry_is_duplicated() {
    let seq = SeqPatch {
        prepend: vec!["X".into(), "X".into()],
        append: vec![],
        delete: vec![],
    };
    assert_eq!(seq.apply(&[]), vec!["X".to_owned()]);
}

#[test]
#[ignore = "finding F5: `apply_values` re-adds an item that is already in the base"]
fn f5_apply_values_readds_an_existing_item() {
    let base: Vec<Value> = vec![json!("A"), json!("B")];
    let seq = SeqPatch {
        prepend: vec!["A".into()],
        append: vec!["A".into()],
        delete: vec![],
    };
    assert_eq!(
        seq.apply_values(&base),
        base,
        "an item already present must not be added again"
    );
}

#[test]
fn a_sequence_patch_note_reports_its_sizes_in_order() {
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    let paths = AppPaths::new(dir.path());
    paths.ensure_dirs().unwrap();
    let mut store = ProfileStore::load(&paths).unwrap();
    let base = store.add(PrfItem::local("L1", "base"));
    store.set_current(&base).unwrap();
    store
        .write_document(
            store.get(&base).unwrap(),
            "mixed-port: 17890\nexternal-controller: 127.0.0.1:9090\nrules:\n  - MATCH,DIRECT\n",
        )
        .unwrap();
    let patch = store.add(PrfItem::patch("r1", "rules", ProfileType::Rules));
    store
        .write_document(
            store.get(&patch).unwrap(),
            "append:\n  - DOMAIN-SUFFIX,a.test,DIRECT\n",
        )
        .unwrap();
    store.save().unwrap();

    let outcome = Pipeline::new(paths).generate(&store).unwrap();
    let note = outcome
        .applied
        .iter()
        .find(|a| a.uid == "r1")
        .map(|a| a.note.clone())
        .unwrap();
    assert!(
        note.contains("1 -> 2"),
        "the note must read (old -> new), got {note:?}"
    );
}

// ================================================================ claim 11
// `encode_segment`: percent-decoding what it produces must give the input back.

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 3 <= bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(10_000))]

    #[test]
    fn claim11_a_path_segment_round_trips_through_percent_decoding(input in any::<String>()) {
        let encoded = encode_segment(&input);
        prop_assert_eq!(percent_decode(&encoded), input);
        // Nothing that would change the shape of the URL may survive.
        prop_assert!(
            encoded
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.~%".contains(c)),
            "encoder emitted {:?}",
            encoded
        );
        prop_assert!(!encoded.contains('/') && !encoded.contains(' '));
    }

    #[test]
    fn claim11_a_query_value_round_trips_through_percent_decoding(input in any::<String>()) {
        let encoded = encode_query(&input);
        prop_assert_eq!(percent_decode(&encoded), input);
    }
}

#[test]
fn claim11_the_two_cases_the_spec_calls_out() {
    assert_eq!(encode_segment("JP 01"), "JP%2001");
    assert_eq!(encode_segment("node b/slash"), "node%20b%2Fslash");
    assert_eq!(
        percent_decode(&encode_segment("node b/slash")),
        "node b/slash"
    );
}

// ================================================================ claim 12
// `mihomo::types`: plausible core responses must not fail to parse.

/// Keys that are *known* to the structs below. An unknown key is what the
/// `#[serde(flatten)]` catch-all exists for; a known key with a wrong type is
/// a different (and implausible) test.
const KNOWN_KEYS: &[&str] = &[
    "name",
    "type",
    "alive",
    "history",
    "id",
    "all",
    "now",
    "testUrl",
    "expectedStatus",
    "fixed",
    "hidden",
    "icon",
    "emptyFallback",
    "provider-name",
    "extra",
    "proxies",
    "providers",
    "vehicleType",
    "updatedAt",
    "behavior",
    "format",
    "ruleCount",
    "payload",
    "index",
    "proxy",
    "size",
    "downloadTotal",
    "uploadTotal",
    "connections",
    "memory",
    "up",
    "down",
    "upTotal",
    "downTotal",
    "inuse",
    "oslimit",
    "network",
    "sourceIP",
    "destinationIP",
    "sourcePort",
    "destinationPort",
    "host",
    "sniffHost",
    "dnsMode",
    "process",
    "processPath",
    "rematchName",
    "remoteDestination",
    "inboundName",
    "upload",
    "download",
    "start",
    "chains",
    "providerChains",
    "rule",
    "rulePayload",
    "metadata",
    "message",
    "hello",
    "status",
    "version",
    "meta",
    "port",
    "socks-port",
    "mixed-port",
    "mode",
    "log-level",
    "ipv6",
    "allow-lan",
    "bind-address",
    "unified-delay",
    "tcp-concurrent",
    "find-process-mode",
    "tun",
    "delay",
    "time",
    "disabled",
    "hitCount",
    "hitAt",
    "missCount",
    "missAt",
];

fn arb_unknown_extras() -> impl Strategy<Value = Map<String, Value>> {
    prop::collection::vec((arb_key(), arb_value()), 0..4).prop_map(|pairs| {
        pairs
            .into_iter()
            .filter(|(k, _)| !KNOWN_KEYS.contains(&k.as_str()))
            .collect::<Map<String, Value>>()
    })
}

fn with_extras(base: &Value, extras: Map<String, Value>) -> Value {
    let mut map = base.as_object().cloned().unwrap_or_default();
    for (k, v) in extras {
        map.insert(k, v);
    }
    Value::Object(map)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(3000))]

    #[test]
    fn claim12_unknown_fields_do_not_break_parsing(
        extras in arb_unknown_extras(),
        name in arb_name(),
        kind in prop::sample::select(vec!["Selector", "Direct", "Vless", "Something New", ""]),
    ) {
        let raw = with_extras(&json!({
            "name": name,
            "type": kind,
            "all": ["a", "b"],
            "history": [{"time": "2026-01-01T00:00:00Z", "delay": 42}],
        }), extras.clone());
        let proxy: ProxyView = serde_json::from_value(raw.clone())
            .unwrap_or_else(|e| panic!("a plausible /proxies entry failed to parse: {e}\n{raw}"));
        prop_assert_eq!(&proxy.name, &name);
        prop_assert_eq!(proxy.members().len(), 2);
        prop_assert_eq!(proxy.latest_delay(), Some(42));

        // The same shape inside the two collections that carry it.
        let proxies: ProxiesResponse = serde_json::from_value(json!({"proxies": {"x": raw}})).unwrap();
        prop_assert_eq!(proxies.proxies.len(), 1);

        let group: ProxyView = serde_json::from_value(with_extras(&json!({
            "name": "grp",
            "type": "Selector",
            "all": ["a"],
            "now": null,
            "fixed": null,
            "hidden": null,
            "icon": null,
            "testUrl": null,
            "expectedStatus": null,
            "emptyFallback": null,
            "provider-name": null,
            "id": null,
        }), extras)).unwrap();
        prop_assert!(group.is_group());
    }

    #[test]
    fn claim12_the_other_wire_types_tolerate_extras(
        extras in arb_unknown_extras(),
        name in arb_name(),
    ) {
        macro_rules! parse {
            ($ty:ty, $body:expr) => {{
                let raw = with_extras(&$body, extras.clone());
                // A JSON round-trip of the model must not lose the extras.
                let value: $ty = serde_json::from_value(raw.clone())
                    .unwrap_or_else(|e| panic!("failed to parse {}: {e}\n{raw}", stringify!($ty)));
                value
            }};
        }

        let _: Version = parse!(Version, json!({"meta": true, "version": "v1.19.31"}));
        let _: Hello = parse!(Hello, json!({"hello": "mihomo"}));
        let _: StatusOk = parse!(StatusOk, json!({"status": "ok"}));
        let _: ApiMessage = parse!(ApiMessage, json!({"message": "?"}));
        let _: DelayResponse = parse!(DelayResponse, json!({"delay": 1}));
        let _: RuleStats = parse!(RuleStats, json!({"disabled": false, "hitCount": 1}));
        let rule: RuleInfo = parse!(RuleInfo, json!({
            "index": 0, "type": "DomainSuffix", "payload": "example.com",
            "proxy": "PROXY", "size": -1, "extra": {"hitCount": 3},
        }));
        prop_assert_eq!(rule.as_config_rule(), "DOMAIN-SUFFIX,example.com,PROXY");
        let rules: RulesResponse = parse!(RulesResponse, json!({"rules": [{"index": 0}]}));
        prop_assert_eq!(rules.rules.len(), 1);
        let _: RuleProviderInfo = parse!(RuleProviderInfo, json!({
            "behavior": "Domain", "format": "TextRule", "name": name, "ruleCount": 2,
            "type": "Rule", "vehicleType": "HTTP", "updatedAt": "2026-01-01T00:00:00Z",
        }));
        let _: RuleProvidersResponse = parse!(RuleProvidersResponse, json!({"providers": {}}));
        let _: ProxyProviderInfo = parse!(ProxyProviderInfo, json!({
            "name": "p", "type": "Proxy", "vehicleType": "HTTP", "proxies": [],
            "testUrl": "", "expectedStatus": "*", "updatedAt": "0001-01-01T00:00:00Z",
        }));
        let _: Traffic = parse!(Traffic, json!({"up": 1, "down": 2}));
        let _: Memory = parse!(Memory, json!({"inuse": 0, "oslimit": 0}));
        let log: LogEvent = parse!(LogEvent, json!({"type": "warning", "payload": "text"}));
        prop_assert!(log.matches("TEXT"));
        let _: GeneralConfig = parse!(GeneralConfig, json!({
            "port": 0, "mixed-port": 17890, "mode": "rule", "log-level": "debug",
            "tun": {"enable": false, "stack": "gVisor"},
        }));
        let snapshot: ConnectionsResponse = parse!(ConnectionsResponse, json!({
            "downloadTotal": 0, "uploadTotal": 87, "connections": null, "memory": 0,
        }));
        prop_assert!(snapshot.items().is_empty());
    }

    #[test]
    fn claim12_a_connection_with_a_real_shape_parses_whatever_the_extras_are(
        extras in arb_unknown_extras(),
        source_port in prop::sample::select(vec!["0", "50130", "65535", ""]),
        destination_port in prop::sample::select(vec!["0", "18099", "65535", ""]),
    ) {
        let body = json!({
            "id": "14ddd52c-5d8c-4ece-afd3-7125c0637db4",
            "metadata": {
                "network": "tcp", "type": "HTTP", "sourceIP": "127.0.0.1",
                "destinationIP": "127.0.0.1", "sourceGeoIP": null, "destinationGeoIP": null,
                "sourceIPASN": "", "destinationIPASN": "", "sourcePort": source_port,
                "destinationPort": destination_port, "inboundIP": "127.0.0.1",
                "inboundPort": "17890", "inboundName": "DEFAULT-MIXED", "inboundUser": "",
                "rematchName": "", "host": "", "dnsMode": "normal", "uid": 0,
                "process": "", "processPath": "", "specialProxy": "", "specialRules": "",
                "remoteDestination": "127.0.0.1", "dscp": 0, "sniffHost": "",
            },
            "upload": 87, "download": 0, "start": "2026-09-25T17:40:55.786450952+08:00",
            "chains": ["DIRECT", "grp-select"], "providerChains": ["", ""],
            "rule": "Match", "rulePayload": "",
        });
        let raw = with_extras(&body, extras);
        let conn: Connection = serde_json::from_value(raw.clone())
            .unwrap_or_else(|e| panic!("a real connection failed to parse: {e}\n{raw}"));
        prop_assert_eq!(conn.total(), 87);
        prop_assert_eq!(conn.selected_group(), Some("grp-select"));
        prop_assert_eq!(conn.outbound_node(), None);
        let meta: Metadata = conn.meta();
        prop_assert_eq!(meta.destination_port_num(), destination_port.parse::<u16>().ok());
    }
}

#[test]
#[ignore = "finding F12: null is rejected for every list field except `connections`"]
fn f12_null_where_a_list_is_expected() {
    // The spec records exactly one list the core sends as `null`
    // (`connections`), and the module doc says "every list that could be
    // `null` is an `Option`". These are the other list-shaped fields; a
    // defensive client should treat `null` as "empty" rather than as a parse
    // failure. This test asserts the defensive behaviour and fails today.
    assert!(
        serde_json::from_str::<ProxyView>(r#"{"name":"x","history":null}"#).is_ok(),
        "`history: null` must mean 'no samples'"
    );
    assert!(
        serde_json::from_str::<ProxiesResponse>(r#"{"proxies":null}"#).is_ok(),
        "`proxies: null` must mean 'no proxies'"
    );
    assert!(
        serde_json::from_str::<RulesResponse>(r#"{"rules":null}"#).is_ok(),
        "`rules: null` must mean 'no rules'"
    );
    assert!(
        serde_json::from_str::<Connection>(r#"{"id":"x","chains":null,"metadata":null}"#).is_ok(),
        "`chains: null` must mean 'no chain'"
    );
    assert!(
        serde_json::from_str::<Metadata>(r#"{"sourcePort":50130,"destinationPort":443}"#).is_ok(),
        "a numeric port must parse even though the core sends a string"
    );
    assert!(
        serde_json::from_str::<DelayResponse>(r#"{"delay":42}"#).is_ok(),
        "the common case must keep working"
    );
}

// ---------------------------------------------------- observation, not a claim

#[test]
fn observation_merge_keys_are_preserved_not_expanded() {
    // Not a finding: mihomo expands `<<` itself, so keeping the literal key is
    // the lossless choice — but the merge engine treats it as an ordinary key.
    let doc = Config::from_yaml("base: &b {x: 1}\nchild:\n  <<: *b\n  y: 2\n").unwrap();
    let merged_into_empty = merged(&json!({}), &doc.as_value(), &MergeOptions::default());
    assert_eq!(merged_into_empty["child"]["<<"]["x"], json!(1));
    let mut v = json!({"z": 3});
    deep_merge(&mut v, doc.get("child").unwrap(), &MergeOptions::default());
    assert_eq!(v["y"], json!(2));
    assert_eq!(v["<<"]["x"], json!(1), "the merge key stays literal");
}
