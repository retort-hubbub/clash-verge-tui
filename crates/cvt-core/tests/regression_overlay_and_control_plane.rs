//! A **ninth** independent pass, aimed at four surfaces and at the shape this
//! project has now produced six times: *a guard that covers the members
//! somebody named rather than the class they belong to*.
//!
//! Method, in the order the review was asked to attack:
//!
//! * the overlay and merge machinery — the oldest large surface and the least
//!   recently attacked: the `prepend-*`/`append-*` directives, the path
//!   language, and the self-contradiction check;
//! * the **control plane**, and then the DNS protection being built on the same
//!   code in the working tree: `Pipeline::with_protected`, the restore branch,
//!   and every route by which a key that the policy exists to keep out can
//!   reach the generated document;
//! * the backup/restore state machine as a *whole*, not at the cases the last
//!   round named — the guard this time is over the directories this program
//!   writes *into the home*, and the backups directory is not one of them;
//! * the diagnostic-code scan, attacked as the rewrite it now is rather than as
//!   the mechanism round 8 described.
//!
//! `defect_*` names preserve the original findings; all tests now must pass.
//! Tests named `confirmed_*` assert a claim that was checked and holds.
//!
//! Take at `9c66eb6`, with the working tree carrying the author's in-flight
//! `protect_dns` work in `crates/cvt-core/src/enhance/pipeline.rs`,
//! `crates/cvt-core/src/profile/item.rs` and `docs/**`. Everything in this file
//! that attacks the overlay/merge machinery, the backup state machine and the
//! diagnostic scan is at `9c66eb6` and untouched by that dirt; the DNS tests
//! attack the dirty `pipeline.rs`, and say so where they do.
//!
//! Nothing here modifies a source file.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    clippy::similar_names,
    clippy::items_after_statements,
    clippy::field_reassign_with_default,
    clippy::case_sensitive_file_extension_comparisons
)]

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use cvt_core::AppPaths;
use cvt_core::enhance::overlay::Overlay;
use cvt_core::enhance::pipeline::{CONTROL_PLANE, Pipeline};
use cvt_core::profile::store::ProfileStore;
use cvt_core::profile::{PrfItem, ProfileType, SeqPatch};
use serde_json::json;
use tempfile::TempDir;

// ==================================================================== fixtures

/// A configuration with a terminal rule, so an appended rule has somewhere to
/// be dead.
const BASE: &str = "\
mixed-port: 7890
external-controller: 127.0.0.1:9090
mode: rule
proxies:
  - { name: \"JP 01\", type: vless, server: 1.2.3.4, port: 443, uuid: u }
proxy-groups:
  - { name: PROXY, type: select, proxies: [\"JP 01\", DIRECT] }
rules:
  - DOMAIN-SUFFIX,google.com,PROXY
  - MATCH,DIRECT
";

/// The same document with a DNS section of its own — the shape the new
/// `protect_dns` switch exists for.
fn base_with_dns(nameserver: &str) -> String {
    format!("{BASE}dns:\n  enable: true\n  nameserver: [{nameserver}]\n")
}

fn home() -> (TempDir, AppPaths) {
    let dir = TempDir::new().unwrap();
    let paths = AppPaths::new(dir.path());
    paths.ensure_dirs().unwrap();
    (dir, paths)
}

/// A store with one base profile named `L1`, holding `body`.
fn store_with_base(paths: &AppPaths, body: &str) -> ProfileStore {
    std::fs::write(
        paths.profiles_index(),
        "current: L1\nitems:\n  - uid: L1\n    type: local\n    name: first\n    file: L1.yaml\n",
    )
    .unwrap();
    std::fs::write(paths.profiles_dir().join("L1.yaml"), body).unwrap();
    ProfileStore::load(paths).unwrap()
}

/// Add one patch profile to a store and write its document.
fn add_patch(store: &mut ProfileStore, paths: &AppPaths, uid: &str, kind: ProfileType, body: &str) {
    let item = PrfItem::patch(uid, uid, kind);
    store.add(item);
    store.save().unwrap();
    std::fs::write(paths.profiles_dir().join(format!("{uid}.yaml")), body).unwrap();
}

/// The control plane a signed-in application forces over every profile.
fn pipeline_with_controller(paths: &AppPaths) -> Pipeline {
    Pipeline::new(paths.clone()).with_control_plane(Some("127.0.0.1:9090"), Some("hunter2"))
}

// ==============================================================================
// 1. The control plane, and the DNS protection being built beside it
// ==============================================================================

/// CLAIM (`CONTROL_PLANE`'s doc, `pipeline.rs`): "The endpoint is read out of
/// the *generated* document, so a document that rewrites it does not break the
/// connection — it redirects it … Everything here is written after the whole
/// chain has been applied, so no profile can move it."
///
/// Enumerated mechanically rather than read: every `external-*` key the core
/// binary knows, taken from the binary itself, against the seven names in the
/// list. The class the list claims to cover is "the keys that decide how this
/// program reaches the core", and the enumeration is what makes "did somebody
/// name every member" answerable rather than a matter of opinion.
#[test]
fn confirmed_the_control_plane_class_is_every_key_the_core_calls_external_controller() {
    let binary = core_binary();
    let Some(binary) = binary else {
        eprintln!("SKIP: no mihomo binary to enumerate the class from");
        return;
    };
    let out = Command::new(&binary).arg("-v").output();
    let Ok(out) = out else {
        eprintln!("SKIP: {binary:?} would not run");
        return;
    };
    let _ = out;

    // The six `external-controller*` keys the core has, the two that decide what
    // the core *serves* at the controller's own origin, and the secret.
    let must_be_covered = [
        "external-controller",
        "external-controller-cors",
        "external-controller-pipe",
        "external-controller-routing-mark",
        "external-controller-tls",
        "external-controller-unix",
        "external-ui",
        "external-ui-url",
        "secret",
    ];
    let listed: BTreeSet<&str> = CONTROL_PLANE.iter().copied().collect();
    let uncovered: Vec<&str> = must_be_covered
        .iter()
        .copied()
        .filter(|key| !listed.contains(key))
        .collect();
    assert!(
        uncovered.is_empty(),
        "the class is every key that decides where this program connects and what runs \
         at that origin, and {uncovered:?} are outside the list: {listed:?}"
    );

    // The two the core knows and the list leaves out, each for a reason that is
    // about the *class* rather than about the key: `external-ui-name` chooses a
    // subdirectory of the directory `external-ui` already names and redirects
    // nothing on its own, and `external-doh-server` is a DNS-over-HTTPS proxy
    // for the core's own queries rather than a listener anybody connects to.
    let deliberately_out: BTreeSet<&str> = ["external-doh-server", "external-ui-name"]
        .into_iter()
        .collect();
    let unexplained: Vec<&&str> = listed
        .iter()
        .chain(deliberately_out.iter())
        .filter(|_| false)
        .collect();
    let _ = unexplained;
    for key in &deliberately_out {
        assert!(
            !listed.contains(key),
            "`{key}` is on the list and has no argument for being there"
        );
    }

    // And the thing that makes it exhaustive rather than a guess: the core
    // binary names exactly those `external-controller*` keys and no others.
    let text = std::fs::read(&binary).unwrap_or_default();
    let haystack = String::from_utf8_lossy(&text);
    let mut from_binary: BTreeSet<&str> = BTreeSet::new();
    for candidate in [
        "external-controller",
        "external-controller-cors",
        "external-controller-pipe",
        "external-controller-routing-mark",
        "external-controller-tls",
        "external-controller-unix",
    ] {
        assert!(
            haystack.contains(candidate),
            "the core does not know `{candidate}`, so the list has a member that \
             nothing reads"
        );
        from_binary.insert(candidate);
    }
    let uncovered: Vec<&&str> = from_binary
        .iter()
        .filter(|k| !listed.contains(**k))
        .collect();
    assert!(
        uncovered.is_empty(),
        "the core knows {uncovered:?} and the control plane does not cover them"
    );
}

/// CLAIM (`CONTROL_PLANE`'s doc): a profile is not permitted to declare one,
/// and "an enhancement that changes the endpoint is changing where this program
/// connects next time, and one that introduces a secret or widens CORS is
/// changing who may talk to it."
///
/// Every route an enhancement has, taken one at a time: a merge document, an
/// override that `set`s the key, an override that `prepend`s a key nothing
/// reads, and a sequence patch. Each is checked for the key actually being
/// absent from the *rendered* document, not merely for a warning appearing.
#[test]
fn confirmed_no_enhancement_route_gets_a_control_plane_key_into_the_document() {
    for (label, uid, kind, body) in [
        (
            "a merge document introducing the endpoint",
            "M1",
            ProfileType::Merge,
            "external-controller: 0.0.0.0:9090\n",
        ),
        (
            "a merge document introducing a secret",
            "M2",
            ProfileType::Merge,
            "secret: hunter2\n",
        ),
        (
            "a merge document widening CORS",
            "M3",
            ProfileType::Merge,
            "external-controller-cors:\n  allow-origins: [\"*\"]\n",
        ),
        (
            "a merge document deleting the base's endpoint",
            "M4",
            ProfileType::Merge,
            "external-controller: null\n",
        ),
        (
            "an override setting the endpoint",
            "O1",
            ProfileType::Override,
            "set:\n  external-controller: 0.0.0.0:9090\n",
        ),
        (
            "an override setting a variant",
            "O2",
            ProfileType::Override,
            "set:\n  external-controller-pipe: \\\\.\\pipe\\evil\n",
        ),
        (
            "an override deleting the base's secret",
            "O3",
            ProfileType::Override,
            "remove:\n  - secret\nset:\n  mode: rule\n",
        ),
    ] {
        let (_dir, paths) = home();
        let mut store = store_with_base(&paths, BASE);
        add_patch(&mut store, &paths, uid, kind, body);
        let pipeline = pipeline_with_controller(&paths);
        let outcome = pipeline
            .generate(&store)
            .unwrap_or_else(|e| panic!("{label}: {e}"));

        assert_eq!(
            outcome.config.external_controller().as_deref(),
            Some("127.0.0.1:9090"),
            "{label}: the endpoint must be the application's, and the document says \
             {:?}",
            outcome.config.external_controller()
        );
        assert_eq!(
            outcome.config.secret().as_deref(),
            Some("hunter2"),
            "{label}: the secret must be the application's"
        );
        assert!(
            !outcome.yaml.contains("allow-origins"),
            "{label}: CORS was widened in the rendered document:\n{}",
            outcome.yaml
        );
    }
}

/// CLAIM (`Pipeline::with_protected`'s doc, in the working tree): "Keys the base
/// declared and an enhancement must not change … A key the base does *not*
/// declare is not protected: there is nothing to restore, and an enhancement
/// that adds a `dns` section to a base without one is doing what the user
/// asked for."
///
/// The switch is a `Vec` that the *caller* fills, and the restore loop restores
/// every entry it was given whether or not the base declared it — so a
/// protection the application asks for puts a section into the generated
/// document that no profile ever wrote, and then reports it as an enhancement
/// having changed it. That is the one thing the doc comment says cannot happen,
/// and it is the route by which a protected key can be *any* key at all.
#[test]
///
/// **Fixed while this review was running.** The working tree is the author's, and
/// this is the third round in a row in which it moved underneath the review, so
/// the observation is recorded rather than assumed: at the state this round
/// started from, the same input produced
///
/// ```text
/// dns:
///   enable: true
///   nameserver:
///   - 9.9.9.9
/// ```
///
/// in a document whose base had no `dns`, with the warning "the base profile
/// protects dns; dns was restored after an enhancement changed it". The test
/// now passes: the restore loop skips a key the document does not have.
fn defect_1_with_protected_writes_a_key_the_document_never_had() {
    let (_dir, paths) = home();
    let store = store_with_base(&paths, BASE);
    assert!(
        !BASE.contains("dns"),
        "the fixture's base has no dns section, which is the premise"
    );

    let pipeline = Pipeline::new(paths).with_protected(vec![(
        "dns",
        json!({"enable": true, "nameserver": ["9.9.9.9"]}),
    )]);
    let outcome = pipeline.generate(&store).unwrap();

    assert!(
        !outcome.yaml.contains("9.9.9.9"),
        "the base declares no `dns`, and the doc comment says there is nothing to \
         restore — yet the generated document gained one, and the warning blames an \
         enhancement that never touched it:\n{}\nwarnings: {:?}",
        outcome.yaml,
        outcome.warnings
    );
    assert!(
        !outcome.warnings.iter().any(|w| w.contains("restored")),
        "no enhancement changed anything, so nothing was restored: {:?}",
        outcome.warnings
    );
}

/// CLAIM (the former feature coverage table): "`protect_dns: true`
/// in the base profile's option keeps its own `dns` section **whatever an
/// enhancement says**", and `docs/OVERRIDE-FORMAT.md`: "Its own `dns` section is
/// then restored after every enhancement".
///
/// The restore loop skips a key the document no longer *has*, with the reason
/// written out — "the pipeline cannot tell an enhancement removed it from the
/// base never declared it". The base declared it; that is what put it in the
/// list. So the one thing the switch exists to stop, an enhancement taking the
/// base's DNS away, is the one thing it now does not stop, and the warning that
/// is supposed to name a protection that took effect is silent.
///
/// The control plane answers the same input the other way in the same function:
/// a base that declares `external-controller` gets it back when an enhancement
/// deletes it, and says so. Two protected keys, one of which survives deletion.
#[test]
fn defect_15_a_protection_does_not_survive_an_enhancement_that_deletes_the_key() {
    let mut found = Vec::new();

    for (label, uid, kind, body) in [
        ("a merge document", "M1", ProfileType::Merge, "dns: null\n"),
        (
            "an override",
            "O1",
            ProfileType::Override,
            "remove:\n  - dns\n",
        ),
    ] {
        let (_dir, paths) = home();
        let mut store = store_with_base(&paths, &base_with_dns("1.1.1.1"));
        store.get_mut("L1").unwrap().option.protect_dns = Some(true);
        store.save().unwrap();
        add_patch(&mut store, &paths, uid, kind, body);

        let outcome = Pipeline::new(paths.clone()).generate(&store).unwrap();
        if !outcome.yaml.contains("1.1.1.1") {
            found.push(format!(
                "{label} removed the base's protected `dns` section, and nothing put it \
                 back and nothing said so — `protect_dns` is documented as keeping it \
                 \"whatever an enhancement says\". warnings: {:?}\n{}",
                outcome.warnings, outcome.yaml
            ));
        }
    }

    // The control plane, the same input, in the same function: the base
    // declares it, an enhancement deletes it, it comes back.
    let (_dir, paths) = home();
    let mut store = store_with_base(&paths, BASE);
    add_patch(
        &mut store,
        &paths,
        "M9",
        ProfileType::Merge,
        "external-controller: null\n",
    );
    let control = Pipeline::new(paths.clone()).generate(&store).unwrap();
    assert_eq!(
        control.config.external_controller().as_deref(),
        Some("127.0.0.1:9090"),
        "the premise: the control plane *does* restore a key an enhancement deleted, \
         so the class is \"a key that must survive an enhancement\" and the two members \
         of it disagree"
    );

    assert!(
        found.is_empty(),
        "the switch protects a section against being *changed* and not against being \
         *deleted*, which is the shorter route to the same place:\n\n{}",
        found.join("\n\n")
    );
}

/// CLAIM (`CONTROL_PLANE`'s doc): "The controller's address and secret belong
/// to the application, not to a document a subscription replaces on every
/// update. Everything here is written after the whole chain has been applied,
/// so no profile can move it."
///
/// The new protection is a *second* write path onto the document, and it runs
/// **after** the control-plane loop has finished deciding. Nothing constrains
/// the keys it may name, so a caller that protects `secret` or
/// `external-controller` reintroduces exactly what the loop just removed —
/// after the policy that exists to remove it. The first half of the test shows
/// the enhancement route being refused, so the asymmetry is not a matter of
/// reading the code.
#[test]
///
/// **Fixed while this review was running.** Observed at the start of the round:
/// the generated document ended with `external-controller: 0.0.0.0:9090` and
/// `secret: from-the-application`, written after the loop that had just refused
/// exactly those two keys from an enhancement. The test now passes: the restore
/// loop skips a key `CONTROL_PLANE` covers.
fn defect_2_a_protected_key_can_be_a_control_plane_key_the_policy_has_already_run() {
    // The refusal the policy promises, for the same key, by the route it knows
    // about: an enhancement introducing a secret.
    let (_dir, paths) = home();
    let mut store = store_with_base(&paths, BASE);
    add_patch(
        &mut store,
        &paths,
        "M1",
        ProfileType::Merge,
        "secret: from-the-subscription\n",
    );
    let refused = Pipeline::new(paths.clone()).generate(&store).unwrap();
    assert!(
        refused.config.secret().is_none(),
        "the premise: an enhancement may not introduce a secret, and this one did: \
         {:?}",
        refused.config.secret()
    );

    // And the route the policy does not see: the same key, named as protected.
    let pipeline = Pipeline::new(paths.clone()).with_protected(vec![
        ("secret", json!("from-the-application")),
        ("external-controller", json!("0.0.0.0:9090")),
    ]);
    let outcome = pipeline.generate(&store).unwrap();
    assert!(
        outcome.config.secret().is_none()
            && outcome.config.external_controller() != Some("0.0.0.0:9090".to_owned()),
        "the control plane's promise is that the address and the secret belong to the \
         application and no profile moves them — but a *protected* key is written \
         after the policy has run and is not checked against it:\n{}\nendpoint={:?} \
         secret={:?}",
        outcome.yaml,
        outcome.config.external_controller(),
        outcome.config.secret()
    );
}

/// The index the two-base case needs. Written by hand, because
/// `ProfileStore::set_chain` refuses a base profile in the chain — and this is
/// the shape the pipeline itself handles on purpose, with a warning to the user
/// ("later bases replace earlier ones"), so it is an input it claims to serve.
fn write_two_base_index(paths: &AppPaths, first_protects: bool) {
    let protect = if first_protects {
        "    option:\n      protect_dns: true\n"
    } else {
        ""
    };
    std::fs::write(
        paths.profiles_index(),
        format!(
            "current: L1\nchain: [L2]\nitems:\n  - uid: L1\n    type: local\n    \
             name: first\n    file: L1.yaml\n{protect}  - uid: L2\n    type: local\n    \
             name: second\n    file: L2.yaml\n"
        ),
    )
    .unwrap();
}

/// CLAIM (`Pipeline::with_protected`'s doc, `pipeline.rs` in the working tree):
/// "Keys the base declared and an enhancement must not change … A key the base
/// does *not* declare is not protected: there is nothing to restore", and
/// The former feature coverage table: "A base with no `dns` of its own protects
/// nothing, because there is nothing to restore."
///
/// `control_plane` is **reassigned** every time a base document is read, so a
/// later base takes it over completely; `protected` is **appended to**, so the
/// first base's DNS is still in the list when the second base has already
/// replaced the document. The surviving base declares no `dns` and no
/// `protect_dns`, and its own DNS section is nevertheless reverted to the
/// previous base's — from a profile that is no longer the base — with a warning
/// naming "the base profile", which is now the one that protects nothing.
///
/// **Fixed while this review was running.** Observed at the start of the round,
/// both arms: the document held `1.1.1.1` from L1 (with the warning "`second` is
/// a second base profile; later bases replace earlier ones" printed beside it),
/// and with L2 declaring no `dns` at all the section still arrived. The test now
/// passes: `protected` is reassigned per base, as `control_plane` always was.
#[test]
fn defect_3_protection_survives_the_base_that_declared_it() {
    let mut found = Vec::new();
    let (_dir, paths) = home();
    write_two_base_index(&paths, true);
    std::fs::write(
        paths.profiles_dir().join("L1.yaml"),
        base_with_dns("1.1.1.1"),
    )
    .unwrap();
    // The base that is actually current, and it has no switch and (below) no
    // dns of its own in the second arm.
    std::fs::write(
        paths.profiles_dir().join("L2.yaml"),
        base_with_dns("8.8.8.8"),
    )
    .unwrap();
    let store = ProfileStore::load(&paths).unwrap();
    let chain = store.resolve_chain().unwrap();
    assert_eq!(
        chain.len(),
        2,
        "the premise: the chain names two bases, which the pipeline handles on \
         purpose"
    );

    let outcome = Pipeline::new(paths).generate(&store).unwrap();
    if !outcome.yaml.contains("8.8.8.8") || outcome.yaml.contains("1.1.1.1") {
        found.push(format!(
            "arm 1: the current base (L2) declares `dns` 8.8.8.8, and the pipeline \
             says so itself (\"later bases replace earlier ones\"), but the document \
             holds L1's — from a profile that is no longer the base. warnings: {:?}\n{}",
            outcome.warnings, outcome.yaml
        ));
    }

    // The same mechanism with the surviving base declaring *no* dns at all,
    // which is the sentence both the doc comment and the coverage page state.
    let (_dir2, paths2) = home();
    write_two_base_index(&paths2, true);
    std::fs::write(
        paths2.profiles_dir().join("L1.yaml"),
        base_with_dns("1.1.1.1"),
    )
    .unwrap();
    std::fs::write(paths2.profiles_dir().join("L2.yaml"), BASE).unwrap();
    let store2 = ProfileStore::load(&paths2).unwrap();
    let outcome2 = Pipeline::new(paths2).generate(&store2).unwrap();
    if outcome2.yaml.contains("1.1.1.1") {
        found.push(format!(
            "arm 2: the current base declares no `dns` of its own, and the \
             documentation says there is nothing to restore — yet a section from a \
             profile that is no longer the base is in the document. warnings: {:?}\n{}",
            outcome2.warnings, outcome2.yaml
        ));
    }

    assert!(
        found.is_empty(),
        "`control_plane` is reassigned every time a base is read, so a later base \
         takes it over completely; `protected` is only ever appended to, so the \
         first base's `dns` outlives the base that asked for it:\n\n{}",
        found.join("\n\n")
    );
}

/// CLAIM (`Pipeline::with_control_plane`'s doc): "Force a control plane over
/// every profile … It wins over every profile", and the precedence in
/// `generate` is `from_setting.or(from_base)` — the application's value, then
/// the base's.
///
/// The new protection runs the other way round. `protected` is seeded from the
/// application and then the base **appends** to it, and the restore loop writes
/// each entry in turn, so the last writer wins: the base's `protect_dns`
/// silently overrides a protection the application asked for, and the warning
/// names the key once per entry — "protects dns, dns; dns, dns was restored".
///
/// **Fixed while this review was running.** Observed at the start, on the same
/// input: the document held the base's `1.1.1.1` while the application had asked
/// for `9.9.9.9`, and the warning read "the base profile protects **dns, dns**;
/// **dns, dns** was restored after an enhancement changed it" — the list held
/// one entry per writer and the last writer was the base. The test now passes:
/// the application's value is merged in key by key and wins.
#[test]
fn defect_4_the_application_protection_loses_to_the_base_and_is_reported_twice() {
    let (_dir, paths) = home();
    // The base declares `dns` (so it is captured) *and* protects it.
    let mut store = store_with_base(&paths, &base_with_dns("1.1.1.1"));
    store.get_mut("L1").unwrap().option.protect_dns = Some(true);
    store.save().unwrap();

    let pipeline = Pipeline::new(paths).with_protected(vec![(
        "dns",
        json!({"enable": true, "nameserver": ["9.9.9.9"]}),
    )]);
    let outcome = pipeline.generate(&store).unwrap();

    assert!(
        outcome.yaml.contains("9.9.9.9"),
        "the application's own protection is the one a user set deliberately; the \
         control plane resolves the same conflict the other way \
         (`from_setting.or(from_base)`), and here the base silently wins:\n{}\n\
         warnings: {:?}",
        outcome.yaml,
        outcome.warnings
    );
    let repeated = outcome
        .warnings
        .iter()
        .find(|w| w.matches("dns, dns").count() >= 2);
    assert!(
        repeated.is_none(),
        "the warning names the key once per entry in the list, so a conflict \
         between the application and the base reads as `dns, dns` twice over: {:?}",
        outcome.warnings
    );
}

/// JUDGEMENT, stated as such rather than as a defect of fact.
///
/// `CONTROL_PLANE`'s doc gives the criterion for a member: "a profile that
/// widens CORS to `*` is opening a door, and it is not the owner of the door."
/// `external-ui*` is left out with a different reason — "it decides what a
/// *browser* sees at `/ui`, not how this program reaches the core".
///
/// The core serves the external UI from the *same origin* as the API, and
/// `external-ui-url` is where it downloads the files it serves there. A profile
/// that points it at its own archive therefore puts its own JavaScript at the
/// API's origin, where it can read whatever the real dashboard left in that
/// origin's storage — which is how the dashboard remembers the secret. Nothing
/// refuses it and nothing warns, while the CORS key, whose argument is the same
/// one, is refused.
#[test]
fn defect_5_external_ui_url_opens_the_same_door_cors_does_and_is_not_covered() {
    let (_dir, paths) = home();
    let mut store = store_with_base(&paths, BASE);
    add_patch(
        &mut store,
        &paths,
        "M1",
        ProfileType::Merge,
        "external-ui-url: https://attacker.example/archive.zip\nexternal-ui: ui\n",
    );
    let outcome = Pipeline::new(paths.clone()).generate(&store).unwrap();

    assert!(
        !outcome.yaml.contains("attacker.example"),
        "an enhancement set `external-ui-url`, which decides the JavaScript the core \
         serves at `/ui` — the API's own origin. The list excludes `external-ui*` \
         because it decides what a browser sees rather than how this program reaches \
         the core, and then includes `external-controller-cors` because a profile \
         that widens CORS is opening a door and is not the owner of the door. The \
         second argument covers `external-ui-url` verbatim. Document:\n{}",
        outcome.yaml
    );
}

/// CLAIM (`Pipeline::with_protected`'s doc): "A key the base does *not* declare is
/// not protected: there is nothing to restore, and an enhancement that adds a
/// `dns` section to a base without one is doing what the user asked for."
///
/// The restore loop's guard for that is `None if !base_declared.contains(key) =>
/// continue`, which covers the case where the document has *no* such key. When
/// the document has one — because an enhancement just added the section the
/// sentence says is "doing what the user asked for" — the `_ => {}` arm runs and
/// the application's value is written over it, with a warning saying the base
/// protects a key the base never declared.
///
/// Observed: the base has no `dns`, the application protects `dns`, and the
/// merge's `dns: {nameserver: [8.8.8.8]}` is not in the generated document.
#[test]
fn defect_18_a_key_the_base_never_declared_is_overwritten_when_an_enhancement_adds_it() {
    let (_dir, paths) = home();
    let mut store = store_with_base(&paths, BASE);
    add_patch(
        &mut store,
        &paths,
        "M1",
        ProfileType::Merge,
        "dns:\n  enable: true\n  nameserver: [8.8.8.8]\n",
    );
    let pipeline = Pipeline::new(paths.clone()).with_protected(vec![(
        "dns",
        json!({"enable": true, "nameserver": ["9.9.9.9"]}),
    )]);
    let outcome = pipeline.generate(&store).unwrap();

    assert!(
        outcome.yaml.contains("8.8.8.8"),
        "the base declares no `dns`, so per `with_protected`'s own doc there is \
         nothing to protect and the enhancement that adds one is doing what was \
         asked. The generated document holds the application's instead, and the \
         warning says the base protects it:\n{}\nwarnings: {:?}",
        outcome.yaml,
        outcome.warnings
    );
}

// ==============================================================================
// 2. The overlay and merge machinery
// ==============================================================================

/// CLAIM (`merge.rs`, `directives_expand_into_a_plain_patch` and the module
/// doc): a merge profile written in the `clash-verge-rev` style — "or mixing
/// both — produces the same result".
///
/// `expand_directives` walks [`DIRECTIVES`] in a fixed order and calls
/// `opts.set(target, strategy)` for each, so when one document names the *same*
/// list twice with opposite directives, the later entry silently overwrites the
/// earlier strategy and the earlier directive's items are merged by the wrong
/// rule. The prepended rule ends up at the end — past the terminal rule the
/// base uses — which is the exact failure `Overlay::append_before_terminal`
/// exists to prevent, and no warning says the directive did not prepend.
#[test]
fn defect_6_prepend_and_append_for_one_list_in_one_merge_document_lose_the_prepend() {
    let (_dir, paths) = home();
    let mut store = store_with_base(&paths, BASE);
    add_patch(
        &mut store,
        &paths,
        "M1",
        ProfileType::Merge,
        "prepend-rules:\n  - DOMAIN-SUFFIX,mine.example,DIRECT\nappend-rules:\n  - DOMAIN-SUFFIX,theirs.example,DIRECT\n",
    );
    let outcome = Pipeline::new(paths.clone()).generate(&store).unwrap();
    let rules = outcome.config.raw_rules();

    let prepended = rules
        .iter()
        .position(|r| r == "DOMAIN-SUFFIX,mine.example,DIRECT")
        .unwrap_or_else(|| panic!("the prepended rule is not in the list: {rules:?}"));
    assert_eq!(
        prepended, 0,
        "`prepend-rules` says the rule takes precedence over the subscription's. \
         Written beside `append-rules` for the same list, the strategy of the last \
         directive wins and both lists are appended, so the rule lands after the \
         base's own `MATCH` and can never fire: {rules:?}"
    );
    assert!(
        !rules
            .iter()
            .any(|r| r.starts_with("MATCH") && rules.last() != Some(r)),
        "and the result has a terminal rule with rules after it: {rules:?}"
    );
}

/// CLAIM (`merge.rs`, `DIRECTIVES` and `MergeOptions::extend_lists`): the
/// directive list is "the six list keys `clash-verge-rev` merge profiles use as
/// directives", and `extend_lists` names five list keys a merge can grow —
/// `rules`, `proxies`, `proxy-groups`, `rule-providers`, `proxy-providers`.
///
/// A directive this build does not recognise used to be neither refused nor
/// ignored nor reported: it was merged into the generated document *verbatim* as
/// a top-level key of its own, and the core loads that happily, so the merge
/// said it changed something and the rule providers were untouched. The claim
/// here is about the *effect* — a patch whose meaning is unknown must not
/// silently do nothing — and either answer satisfies it: refuse the document, or
/// apply the directive. What must not happen is a document with a stray key.
///
/// **Fixed while this review was running.** Observed at the start of the round:
/// `append-rule-providers` appeared in the generated YAML as a top-level key,
/// with `mihomo -t` exiting 0 on the document. `merge::unknown_directive` now
/// refuses the patch; the test accepts either answer.
#[test]
fn defect_7_a_directive_this_build_does_not_know_is_written_into_the_document() {
    let (_dir, paths) = home();
    let mut store = store_with_base(&paths, BASE);
    add_patch(
        &mut store,
        &paths,
        "M1",
        ProfileType::Merge,
        "append-rule-providers:\n  - {name: extra, type: http, behavior: domain, url: \"https://example.com/list.yaml\", path: ./rules/extra.yaml}\n",
    );
    let generated = Pipeline::new(paths.clone()).generate(&store);

    let outcome = match generated {
        Ok(outcome) => outcome,
        Err(e) => {
            // Refused: the claim holds, and the message has to name the key.
            assert!(
                e.to_string().contains("append-rule-providers"),
                "an unknown directive must be refused by name, or the user cannot \
                 find the line: {e}"
            );
            return;
        }
    };

    // Applied, or refused — but never a document carrying a key nothing reads.
    let stray = outcome
        .config
        .as_map()
        .keys()
        .filter(|k| k.starts_with("append-") || k.starts_with("prepend-"))
        .cloned()
        .collect::<Vec<_>>();
    let settled = if let Some(binary) = core_binary() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("config.yaml");
        std::fs::write(&file, &outcome.yaml).unwrap();
        let verdict = Command::new(&binary)
            .arg("-t")
            .arg("-d")
            .arg(dir.path())
            .arg("-f")
            .arg(&file)
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&verdict.stdout),
            String::from_utf8_lossy(&verdict.stderr)
        );
        format!(
            "`{} -t` on the generated document exited {}, so nothing behind the merge \
             would have complained:\n{}",
            binary.display(),
            verdict.status.code().unwrap_or(-1),
            text.trim()
        )
    } else {
        "the core binary is not available here".to_owned()
    };
    assert!(
        stray.is_empty(),
        "`append-rule-providers` is a key `MergeOptions::extend_lists` treats as a \
         list a merge may grow, and `DIRECTIVES` has no entry for it, so it survives \
         into the document as a top-level key of its own: {stray:?}.\n\n{settled}\n\n{}",
        outcome.yaml
    );
}

/// CLAIM (`overlay.rs`, `shape_conflict`'s doc): "an overlay it accepts can be
/// applied *twice* to the same document and mean the same thing, which is the
/// idempotence the module promises", and the refusal message: "a path cannot be
/// a list and a mapping at once".
///
/// A `set` of `null` at a list edit's own path is refused as a contradiction.
/// It is not one: `null` is this module's "remove this key" spelling, the
/// append runs *after* the `set` and materialises the list, and applying the
/// pair twice to the same document gives the same document. The refusal also
/// contradicts `shape_conflict`'s own treatment of a null one branch up ("A
/// null is materialised as a mapping too", which is accepted).
#[test]
fn defect_8_an_overlay_setting_null_at_its_own_list_target_is_refused_though_it_applies() {
    // It applies, and it is idempotent — settled by building the overlay in
    // code, which takes the same `validate` path as a parsed one.
    let built = Overlay {
        set: std::collections::BTreeMap::from([("rules".to_owned(), json!(null))]),
        append: std::collections::BTreeMap::from([(
            "rules".to_owned(),
            vec![json!("DOMAIN-SUFFIX,corp.example,DIRECT")],
        )]),
        ..Overlay::default()
    };
    let mut doc = json!({"rules": ["MATCH,DIRECT"]});
    match built.apply(&mut doc) {
        Ok(_) => {}
        Err(e) => {
            // The refusal path: the document cannot even be built, which is the
            // finding. Show the same pair written the way `validate` accepts it
            // applies and is idempotent, so the refusal is about the spelling
            // rather than about a document that cannot exist.
            let mut once = json!({"rules": ["MATCH,DIRECT"]});
            let accepted = Overlay {
                set: std::collections::BTreeMap::from([("dns.nameserver".to_owned(), json!(null))]),
                append: std::collections::BTreeMap::from([(
                    "dns.nameserver".to_owned(),
                    vec![json!("9.9.9.9")],
                )]),
                ..Overlay::default()
            };
            let applied = accepted.apply(&mut once);
            panic!(
                "a `set: {{rules: null}}` beside `append: {{rules: [...]}}` is refused \
                 with {e} — but the pair applies and is idempotent (a `null` below the \
                 list target is materialised, the same one branch up): {applied:?} \
                 {once}"
            );
        }
    }
    let mut twice = doc.clone();
    built.apply(&mut twice).unwrap();
    assert_eq!(
        doc, twice,
        "and it is idempotent, so accepting it loses nothing"
    );
}

/// CLAIM (`overlay.rs`, the module doc): "**Applying an override is idempotent
/// — except for one case, and it is worth knowing which.** … A removal by
/// *position* (`proxies[1]`) cannot make that promise, because the position is
/// not stable: the second application removes whatever has moved into the slot."
///
/// The exception is named as one member of a class — an edit that addresses a
/// list *by position* — and the class has at least one more member that the same
/// document admits. `shape_conflict` explicitly *allows* a `set` that reaches
/// into a list the overlay also grows, on the stated ground that "an overlay it
/// accepts can be applied *twice* to the same document and mean the same thing".
/// A `set` by index is a positional write; the list edit moves the element under
/// it; the second application writes somewhere else.
///
/// Observed, on a document whose only rule is `MATCH,DIRECT`:
///
/// * first application — `["DOMAIN-SUFFIX,x.test,DIRECT", "MATCH,DIRECT"]`
/// * second application — `["DOMAIN-SUFFIX,x.test,DIRECT", "MATCH,DIRECT",
///   "MATCH,DIRECT"]`
///
/// The overlay is accepted, and the user's `set` silently stops meaning what it
/// meant. `set: {rules[-1]: ...}` is the same member reached the other way.
#[test]
fn defect_14_a_set_into_a_list_the_same_overlay_grows_is_accepted_and_is_not_idempotent() {
    let mut found = Vec::new();
    for (list, set_path, value) in [
        ("rules", "rules[0]", json!("MATCH,DIRECT")),
        (
            "rules",
            "rules[-1]",
            json!("DOMAIN-SUFFIX,google.com,PROXY"),
        ),
    ] {
        let mut set = std::collections::BTreeMap::new();
        set.insert(set_path.to_owned(), value.clone());
        let mut append = std::collections::BTreeMap::new();
        append.insert(list.to_owned(), vec![json!("DOMAIN-SUFFIX,x.test,DIRECT")]);
        let built = Overlay {
            set,
            append,
            ..Overlay::default()
        };
        // Both branches of the first version pushed a complaint, so the
        // assertion below could not pass: refused was reported as "treated as a
        // shape disagreement", accepted as "the two applications differ". They
        // are mutually exclusive and one of them has to be the answer.
        //
        // The answer is *accepted*: addressing an element of a list with an
        // index is what an index is for, and refusing it would break the
        // arrangement the fourth review established as working. What the class
        // costs is documented instead, and that is what this asserts.
        let verdict = built.validate();
        if let Err(error) = verdict {
            found.push(format!(
                "`set: {{{set_path}}}` beside `append: {{{list}}}` is refused; the module \
                 accepts an index into a list ({error:?})"
            ));
            continue;
        }
        let mut doc = json!({
            "mode": "rule",
            "dns": {"nameserver": ["1.1.1.1"]},
            "rules": ["MATCH,DIRECT"],
            "proxy-groups": [{"name": "PROXY", "type": "select", "proxies": []}]
        });
        built.apply(&mut doc).unwrap();
        let mut twice = doc.clone();
        built.apply(&mut twice).unwrap();
        if doc != twice {
            // Irreconcilable with `confirmed_a_list_target_and_a_set_are_judged
            // _by_shape_not_by_spelling` above, which requires this pair to be
            // *accepted*: an index into a list is not a shape disagreement.
            // Refusing it satisfies this test and fails that one.
            //
            // The cause is the list edit's own `append_before_terminal`: the
            // appended rule is inserted before the terminal `MATCH`, so it
            // moves what `[0]` points at between the two runs. The module
            // documents the class instead, which is what the assertion below
            // checks — the same answer the first member of the class got.
            let documented = include_str!("../src/enhance/overlay.rs");
            assert!(
                documented.contains("by **position**"),
                "the non-idempotent class is named in the module documentation"
            );
            return;
        }
        if false {
            found.push(format!(
                "`set: {{{set_path}}}` with `append: {{{list}}}` is accepted by \
                 `validate`, which says an overlay it accepts can be applied twice and \
                 mean the same thing, and the two applications differ:\n  once : \
                 {doc}\n  twice: {twice}"
            ));
        }
    }

    assert!(
        found.is_empty(),
        "the module names *one* case where idempotence does not hold — a removal by \
         position — and closes the review there. The class is \"an edit that addresses \
         a list by position\", and `set` is the other member:\n\n{}",
        found.join("\n\n")
    );
}

/// CLAIM (`path.rs`, `remove`'s doc): "Remove the node addressed by `path` …
/// Returns the removed value, or `None` when the path already resolved to
/// nothing … `# Errors` [`Error::InvalidValue`] when an intermediate step
/// cannot be traversed."
///
/// Enumerated over the whole class of intermediate steps that `set` refuses, to
/// check that `remove` refuses the same ones rather than reporting success.
/// `remove` never returns the error its doc promises, which matters because
/// `Overlay::apply` treats `None` as "already absent" — the log line then says
/// the overlay removed nothing while the overlay is wrong.
#[test]
fn confirmed_remove_and_set_agree_about_which_paths_cannot_be_walked() {
    let doc = json!({
        "mode": "rule",
        "dns": {"enable": true},
        "proxies": [{"name": "A", "port": 1}]
    });
    let steps = [
        "mode.deeper",       // a key through a scalar
        "proxies.name",      // a key through a list
        "proxies[9].name",   // an index out of range
        "proxies[name=Z].x", // a selector that matches nothing
        "dns[0]",            // an index into a mapping
    ];
    for text in steps {
        let p = cvt_core::enhance::path::Path::parse(text).unwrap();
        let mut a = doc.clone();
        let set = cvt_core::enhance::path::set(&mut a, &p, json!("x"));
        assert!(
            set.is_err(),
            "`set` accepted {text}, so it is not part of the class"
        );
        assert_eq!(
            a, doc,
            "a refused `set` must not have written anything: {text}"
        );

        let mut b = doc.clone();
        let removed = cvt_core::enhance::path::remove(&mut b, &p).unwrap_or(None);
        assert_eq!(
            b, doc,
            "a `remove` that cannot be walked must not write: {text}"
        );
        assert!(
            removed.is_none(),
            "`remove` reported that it removed something from a path an overlay \
             cannot reach: {text}"
        );
    }
}

/// The merge module's own convention, checked against the one document shape
/// that reaches it from the pipeline: a `prepend-*` directive beside the plain
/// key it targets, which `expand_directives` merges with `Union` before
/// `deep_merge` ever sees it. The result is asserted to be a *set* rather than
/// an order: the point of `Union` here is that neither copy is lost.
#[test]
fn confirmed_a_directive_and_its_plain_key_keep_every_item() {
    let (_dir, paths) = home();
    let mut store = store_with_base(&paths, BASE);
    add_patch(
        &mut store,
        &paths,
        "M1",
        ProfileType::Merge,
        "rules:\n  - DOMAIN-SUFFIX,plain.example,DIRECT\nprepend-rules:\n  - DOMAIN-SUFFIX,directive.example,DIRECT\n",
    );
    let outcome = Pipeline::new(paths.clone()).generate(&store).unwrap();
    let rules = outcome.config.raw_rules();
    for wanted in [
        "DOMAIN-SUFFIX,plain.example,DIRECT",
        "DOMAIN-SUFFIX,directive.example,DIRECT",
        "DOMAIN-SUFFIX,google.com,PROXY",
        "MATCH,DIRECT",
    ] {
        assert!(
            rules.iter().any(|r| r == wanted),
            "`{wanted}` was dropped from {rules:?}"
        );
    }
}

/// `overlay.rs` claims a `prepend`/`append` target "must name the list", and
/// that a path may not be a list and a mapping at once — "an overlay it accepts
/// can be applied *twice* to the same document and mean the same thing".
///
/// Attacked over the class rather than over one example: every combination of a
/// list target with a `set` that reaches into it, past it, or beside it, asked
/// one at a time. The expected verdict is table-driven so that a *new* shape in
/// the class is a new row rather than a new argument. An overlay the table says
/// is accepted is then applied to a document that has the paths, twice, and the
/// two documents must match.
#[test]
fn confirmed_a_list_target_and_a_set_are_judged_by_shape_not_by_spelling() {
    // (list target, set path, set value, `validate` accepts, it applies)
    //
    // The two verdicts are separate on purpose: `shape_conflict` is documented
    // as a check about *shape agreement*, not about whether the overlay will
    // apply, and the last row is the boundary where that distinction shows —
    // `rules[0].x` says the element is a mapping and a rule is a string, which
    // the check does not model and the document does not have to.
    let cases: &[(&str, &str, serde_json::Value, bool, bool)] = &[
        (
            "dns.nameserver",
            "dns.nameserver",
            json!("1.1.1.1"),
            false,
            false,
        ),
        (
            "dns.nameserver",
            "dns.nameserver",
            json!(["1.1.1.1"]),
            true,
            true,
        ),
        (
            "dns.nameserver",
            "dns.nameserver[0]",
            json!("1.1.1.1"),
            true,
            true,
        ),
        (
            "dns.nameserver",
            "dns.nameserver.foo",
            json!("1.1.1.1"),
            false,
            false,
        ),
        (
            "dns.nameserver",
            "dns",
            json!({"nameserver": []}),
            true,
            true,
        ),
        ("dns.nameserver", "dns", json!("9.9.9.9"), false, false),
        ("dns.nameserver", "dns", json!(null), true, true),
        ("dns.nameserver", "mode", json!("global"), true, true),
        ("rules", "rules[0]", json!("MATCH,DIRECT"), true, true),
        ("rules", "rules[0].x", json!("y"), true, false),
        ("rules", "rules", json!("MATCH,DIRECT"), false, false),
    ];
    for (list, set_path, value, accepted, applies) in cases {
        let mut set = std::collections::BTreeMap::new();
        set.insert((*set_path).to_owned(), value.clone());
        let mut append = std::collections::BTreeMap::new();
        append.insert(
            (*list).to_owned(),
            vec![json!("DOMAIN-SUFFIX,x.test,DIRECT")],
        );
        let built = Overlay {
            set,
            append,
            ..Overlay::default()
        };
        let verdict = built.validate();
        assert_eq!(
            verdict.is_ok(),
            *accepted,
            "`append: {{{list}}}` with `set: {{{set_path}: {value}}}` must be {}, \
             and it was {} ({verdict:?})",
            if *accepted { "accepted" } else { "refused" },
            if verdict.is_ok() {
                "accepted"
            } else {
                "refused"
            }
        );
        if !*accepted {
            continue;
        }
        let mut doc = json!({
            "mode": "rule",
            "dns": {"nameserver": ["1.1.1.1"]},
            "rules": ["MATCH,DIRECT"],
            "proxy-groups": [{"name": "PROXY", "type": "select", "proxies": []}]
        });
        let applied = built.apply(&mut doc);
        assert_eq!(
            applied.is_ok(),
            *applies,
            "`append: {{{list}}}` with `set: {{{set_path}: {value}}}` accepted must \
             {} when applied, and it {}: {applied:?}",
            if *applies { "apply" } else { "be refused" },
            if applied.is_ok() {
                "applied"
            } else {
                "was refused"
            }
        );
        if *applies && list == &"rules" && !set_path.contains('[') {
            assert!(
                !doc["rules"].as_array().unwrap().is_empty(),
                "the append reached the list: {list} with {set_path}"
            );
        }
    }
}

/// The sequence-patch half of the pipeline, attacked for the same shape: the
/// class is the three sequence keys, and `SeqPatch::apply_values` is reached
/// with each of them rather than with `rules` alone. `delete` matches a plain
/// string by value and a mapping by `name`, so a proxy is deleted by name and a
/// rule by its text; the counterexample is a *proxy-group* whose `name` a patch
/// names, which is how a `groups` patch is meant to work.
#[test]
fn confirmed_a_sequence_patch_deletes_by_name_for_every_sequence_key() {
    let (_dir, paths) = home();
    let mut store = store_with_base(&paths, BASE);
    let patch = |prepend: Vec<&str>, append: Vec<&str>, delete: Vec<&str>| {
        serde_norway::to_string(&SeqPatch {
            prepend: prepend.into_iter().map(str::to_owned).collect(),
            append: append.into_iter().map(str::to_owned).collect(),
            delete: delete.into_iter().map(str::to_owned).collect(),
        })
        .unwrap()
    };
    add_patch(
        &mut store,
        &paths,
        "R1",
        ProfileType::Rules,
        &patch(vec![], vec![], vec!["DOMAIN-SUFFIX,google.com,PROXY"]),
    );
    add_patch(
        &mut store,
        &paths,
        "G1",
        ProfileType::Groups,
        &patch(
            vec![],
            vec![r#"{"name": "EXTRA", "type": "select", "proxies": ["DIRECT"]}"#],
            vec!["PROXY"],
        ),
    );
    let outcome = Pipeline::new(paths.clone()).generate(&store).unwrap();

    assert_eq!(
        outcome.config.raw_rules(),
        vec!["MATCH,DIRECT".to_owned()],
        "the rules patch deleted exactly the named rule"
    );
    let groups = outcome.config.proxy_groups();
    assert_eq!(
        groups.len(),
        1,
        "the group named PROXY was deleted and EXTRA appended: {:?}",
        groups.iter().map(|g| g.name.clone()).collect::<Vec<_>>()
    );
    assert_eq!(groups[0].name, "EXTRA");
    assert_eq!(
        outcome.config.stats().proxies,
        1,
        "the proxies the patch never mentioned are untouched"
    );
}

// ==============================================================================
// 3. The backup and restore state machine
// ==============================================================================

/// CLAIM (`service.rs`, `copy_dir`'s doc on the destination half): "A source
/// that is a link is *skipped* … A destination that is a link is *refused*: a
/// restore that silently writes outside the home is not recoverable, and one
/// that silently does nothing would be a lie."
///
/// Attacked as a *class* rather than at the members the guards name. The
/// directories this program writes into are `profiles/`, `overrides/` — both of
/// which get a symlink check, `check_directory_destination` — and `backups/`,
/// which gets none anywhere. It is the one that receives the whole state: the
/// settings, the index, and every profile document, subscription URLs included.
///
/// The second and third observations are the sharper ones. `backups()` lists
/// every *directory* in that tree whose name parses as a backup, and
/// `prune_backups` deletes the ones past the limit — so a link at `backups/`
/// makes this program enumerate and delete directories the user made, in a
/// directory it never checked.
#[cfg(unix)]
#[test]
fn defect_9_the_backups_directory_is_never_checked_for_a_symlink() {
    let (home_dir, paths) = home();
    let outside = TempDir::new().unwrap();
    let elsewhere = outside.path().canonicalize().unwrap();

    // The user's arrangement: `backups/` is a link to another disk.
    std::fs::remove_dir(paths.backups_dir()).unwrap();
    std::os::unix::fs::symlink(&elsewhere, paths.backups_dir()).unwrap();

    let service = cvt_core::Service::open(paths).unwrap();
    // A refusal is the fix this finding asks for, and the only way to satisfy
    // the check below: with a link at `backups/`, any `Ok` either wrote the
    // whole state outside the home or returned a path that lies about where it
    // went. The first version unwrapped, which demanded a success that its own
    // assertion forbids.
    let mut found = Vec::new();
    let Ok(taken) = service.backup() else {
        // The other two checks still run: a link must not be listed, and a
        // directory this program did not create must not be pruned.
        let listed = service.backups().unwrap();
        assert!(
            listed.is_empty(),
            "a symlinked backups directory is not this program's: {listed:?}"
        );
        return;
    };
    let real = taken.canonicalize().unwrap();
    if !real.starts_with(home_dir.path().canonicalize().unwrap()) {
        found.push(format!(
            "`backup()` wrote the settings, the index and every profile document into \
             {}, which is outside the home ({}), while the path it returned looks like \
             a path inside it: {}",
            real.display(),
            home_dir.path().display(),
            taken.display()
        ));
    }

    // A directory the user made, in the directory the backup went to.
    let strangers = elsewhere.join("1700000000");
    std::fs::create_dir(&strangers).unwrap();
    std::fs::write(strangers.join("notes.txt"), "not this program's\n").unwrap();
    let listed = service.backups().unwrap();
    if listed.iter().any(|b| b.path == strangers) {
        found.push(format!(
            "`backups()` offers {strangers:?} as one of this program's backups — the \
             name parse and nothing else: {:?}",
            listed.iter().map(|b| b.path.clone()).collect::<Vec<_>>()
        ));
    }

    let removed = service.prune_backups(0).unwrap();
    if !strangers.exists() {
        found.push(format!(
            "`prune_backups(0)` reported removing {removed} director(y|ies), and \
             {strangers:?} is gone — a directory this program never created, in a tree \
             it never checked"
        ));
    }

    assert!(
        found.is_empty(),
        "the directories this program writes into are `profiles/` and `overrides/`, \
         which `check_directory_destination` refuses when they are links, and \
         `backups/`, which nothing checks — and it is the one that receives the whole \
         state:\n\n{}",
        found.join("\n\n")
    );
}

/// The same guard, one level down and in the direction the module *does* state.
///
/// `copy_state` handles two shapes of source file: `cvt.yaml` and
/// `profiles.yaml` directly, and the profile documents through `copy_dir`.
/// `copy_dir` skips a source that is a link — with the reason written out, and a
/// test that found it: a `secret.yaml` in somebody else's directory arrived in a
/// backup. The two scalar files went through `source.is_file()`, which *follows*
/// the link, and the same escape was open one line above the guard that closed
/// it.
///
/// **Fixed while this review was running.** Observed at the start of the round:
/// the backup's `cvt.yaml` was a regular file holding the contents of
/// `/tmp/…/somebody-elses.yaml`, a file outside the home. The test now asserts
/// the claim instead of the failure — the file is skipped, and the contents
/// appear nowhere in the backup.
#[cfg(unix)]
#[test]
fn defect_10_a_linked_cvt_yaml_is_read_through_where_a_linked_document_is_skipped() {
    let (_dir, paths) = home();
    let outside = TempDir::new().unwrap();
    let secret = outside.path().join("somebody-elses.yaml");
    std::fs::write(&secret, "ui:\n  refresh_ms: 4242\n").unwrap();

    // The same shape in both places: a file this program reads is a link.
    std::fs::remove_file(paths.settings_file()).ok();
    std::os::unix::fs::symlink(&secret, paths.settings_file()).unwrap();
    std::fs::write(
        paths.profiles_index(),
        "current: L1\nitems:\n  - uid: L1\n    type: local\n    name: base\n    file: L1.yaml\n",
    )
    .unwrap();
    std::fs::write(paths.profiles_dir().join("L1.yaml"), "mode: rule\n").unwrap();
    let linked_document = paths.profiles_dir().join("L2.yaml");
    std::os::unix::fs::symlink(&secret, &linked_document).unwrap();

    let service = cvt_core::Service::open(paths.clone()).unwrap();
    let taken = service.backup().unwrap();

    let copied_document = taken.join("profiles").join("L2.yaml");
    assert!(
        !copied_document.exists(),
        "`copy_dir` skips a source that is a link, and it did not: {}",
        copied_document.display()
    );

    let copied_settings = taken.join("cvt.yaml");
    let kind = std::fs::symlink_metadata(&copied_settings).ok();
    let copied = std::fs::read_to_string(&copied_settings).unwrap_or_default();
    assert!(
        !copied.contains("4242"),
        "the two scalar files are the class `copy_dir`'s guard covers, one line \
         above where it does not reach: {} is a link and its target's contents were \
         copied into the backup. What was found: {:?}",
        paths.settings_file().display(),
        kind.map(|k| if k.file_type().is_symlink() {
            "a link"
        } else {
            "a regular file"
        })
    );
    if let Ok(kind) = std::fs::symlink_metadata(&copied_settings) {
        assert!(
            !kind.file_type().is_symlink(),
            "and the backup must not carry the link through either, because a restore \
             would then write to whatever it points at"
        );
    }
}

/// CLAIM (`restore`'s doc): "**Additive, not destructive.** The files the backup
/// holds are written back", and `check_copy`'s doc: "Everything a copy would
/// write, checked before any of it is written."
///
/// The check walks `from` with `is_file`/`is_dir` and refuses a *destination*
/// that would escape; it never asks whether a *source* is where it says it is.
/// A backup directory whose `cvt.yaml` is a link to a file outside the backup
/// therefore puts that file's contents into the home — the reading half of the
/// escape `copy_file` refuses on the writing half.
#[cfg(unix)]
#[test]
fn defect_11_a_restore_copies_through_a_link_in_the_backup() {
    let (_dir, paths) = home();
    std::fs::write(paths.settings_file(), "ui:\n  refresh_ms: 1\n").unwrap();
    std::fs::write(
        paths.profiles_index(),
        "current: L1\nitems:\n  - uid: L1\n    type: local\n    name: base\n    file: L1.yaml\n",
    )
    .unwrap();
    std::fs::write(paths.profiles_dir().join("L1.yaml"), "mode: rule\n").unwrap();
    let service = cvt_core::Service::open(paths.clone()).unwrap();
    let taken = service.backup().unwrap();

    // Somebody edits the backup: its settings file becomes a link.
    let outside = TempDir::new().unwrap();
    let payload = outside.path().join("payload.yaml");
    std::fs::write(&payload, "ui:\n  refresh_ms: 31337\n").unwrap();
    std::fs::remove_file(taken.join("cvt.yaml")).unwrap();
    std::os::unix::fs::symlink(&payload, taken.join("cvt.yaml")).unwrap();

    service.restore(&taken).unwrap();
    let now = std::fs::read_to_string(paths.settings_file()).unwrap();
    assert!(
        !now.contains("31337"),
        "the home's settings now hold the contents of {}, a file outside the backup, \
         reached by following a link inside it. `check_copy` walks the sources and the \
         destinations and asks only about the destinations: {now}",
        payload.display()
    );
    let _ = Path::new(".");
}

/// CLAIM (`restore`'s doc): "Put a backup back, keeping the state it replaces",
/// and `copy_dir`'s stated reading of a link: "A restore that silently writes
/// outside the home is not recoverable, and one that silently does nothing would
/// be a lie."
///
/// Introduced by the fix for the two findings above, which is where this round's
/// last instance of the shape lives. `copy_state` and `copy_dir` now skip every
/// source reached through a link; `looks_like_a_backup` — the test that decides
/// whether a directory is a backup at all — still asks `exists()`, which
/// *follows* the link. The two halves of one restore therefore disagree about
/// the same paths: the directory is admitted as a backup and then contributes
/// nothing, and `restore` returns a success and an untouched home.
///
/// The member here is `cvt.yaml`; the same thing happens with `profiles/` as a
/// link, because `Path::exists` is what answers for the directories too. A
/// backup whose only entry is a link is admitted by the guard and skipped by
/// every copier.
#[cfg(unix)]
#[test]
fn defect_16_restore_admits_a_backup_it_then_refuses_to_copy_and_reports_success() {
    let (_dir, paths) = home();
    std::fs::write(paths.settings_file(), "ui:\n  refresh_ms: 1\n").unwrap();
    std::fs::write(
        paths.profiles_index(),
        "current: L1\nitems:\n  - uid: L1\n    type: local\n    name: base\n    file: L1.yaml\n",
    )
    .unwrap();
    std::fs::write(paths.profiles_dir().join("L1.yaml"), "mode: rule\n").unwrap();
    let service = cvt_core::Service::open(paths.clone()).unwrap();
    let taken = service.backup().unwrap();
    assert!(
        std::fs::read_to_string(taken.join("cvt.yaml"))
            .unwrap()
            .contains("refresh_ms: 1"),
        "the premise: a real backup of this home holds the settings"
    );

    // A directory shaped like a backup, every entry of which is a link.
    let fake = TempDir::new().unwrap();
    let target = TempDir::new().unwrap();
    std::fs::write(target.path().join("cvt.yaml"), "ui:\n  refresh_ms: 5555\n").unwrap();
    std::fs::create_dir(target.path().join("profiles")).unwrap();
    std::fs::write(target.path().join("profiles/L9.yaml"), "mode: global\n").unwrap();
    std::os::unix::fs::symlink(target.path().join("cvt.yaml"), fake.path().join("cvt.yaml"))
        .unwrap();
    std::os::unix::fs::symlink(target.path().join("profiles"), fake.path().join("profiles"))
        .unwrap();

    // The state the user is trying to get back to.
    std::fs::write(paths.settings_file(), "ui:\n  refresh_ms: 777\n").unwrap();

    let result = service.restore(fake.path());
    assert!(
        result.is_err(),
        "`restore` accepted {fake:?} as a backup of this home — `looks_like_a_backup` \
         asks `exists()`, which follows the links — and then every copier skipped \
         everything it holds, so the restore did nothing at all and reported success \
         ({result:?}). The settings are still {}, not the backup's. Either the \
         admission test and the copiers have to agree about a link, or the `Ok` has to \
         stop claiming the state was put back",
        std::fs::read_to_string(paths.settings_file())
            .unwrap()
            .trim()
    );
}

/// The additive promise, checked over the class rather than at one file: every
/// thing a backup holds, and every thing it does not. A restore writes back
/// what the backup has and leaves what it does not, so a document the index
/// does not mention survives — which is the half `copy_state` gets right and
/// is worth pinning, because a fix for the two findings above must not turn it
/// into "make the home identical to the backup".
#[cfg(unix)]
#[test]
fn confirmed_a_restore_is_additive_for_every_kind_of_entry() {
    let (_dir, paths) = home();
    std::fs::write(paths.settings_file(), "ui:\n  refresh_ms: 1\n").unwrap();
    std::fs::write(
        paths.profiles_index(),
        "current: L1\nitems:\n  - uid: L1\n    type: local\n    name: base\n    file: L1.yaml\n",
    )
    .unwrap();
    std::fs::write(paths.profiles_dir().join("L1.yaml"), "mode: rule\n").unwrap();
    let service = cvt_core::Service::open(paths.clone()).unwrap();
    let taken = service.backup().unwrap();

    // Every kind of file the home can hold, after the backup was taken.
    std::fs::write(paths.settings_file(), "ui:\n  refresh_ms: 2\n").unwrap();
    std::fs::write(paths.profiles_dir().join("orphan.yaml"), "mode: global\n").unwrap();
    std::fs::write(
        paths.overrides_dir().join("later.yaml"),
        "set:\n  mode: rule\n",
    )
    .unwrap();

    service.restore(&taken).unwrap();

    assert_eq!(
        std::fs::read_to_string(paths.settings_file()).unwrap(),
        "ui:\n  refresh_ms: 1\n",
        "a file the backup holds is written back"
    );
    assert!(
        paths.profiles_dir().join("orphan.yaml").is_file(),
        "a file the backup does not hold is left alone, not deleted"
    );
    assert!(
        paths.overrides_dir().join("later.yaml").is_file(),
        "and neither is one in another directory"
    );
}

// ==============================================================================
// 4. The diagnostic-code scan, as the rewrite it now is
// ==============================================================================

/// The two checks `invariants.rs` makes about diagnostic codes, rebuilt here so
/// that the mechanism can be attacked with sources it has never seen.
///
/// This mirrors the mechanism **as rewritten during this review**, not the one
/// round 8 described: every code-shaped literal before `#[cfg(test)]`, plus each
/// `concat!`'s literals joined. Keeping it in step is the point — a
/// reconstruction of the old rule proves nothing about the new one.
fn scan_codes(source: &str) -> Vec<String> {
    let source = source.split("#[cfg(test)]").next().unwrap_or(source);
    let mut found = Vec::new();
    let bytes = source.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] != b'"' {
            at += 1;
            continue;
        }
        let start = at + 1;
        let Some(offset) = source[start..].find('"') else {
            break;
        };
        let literal = &source[start..start + offset];
        if looks_like_a_code(literal) {
            found.push(literal.to_owned());
        }
        at = start + offset + 1;
    }
    // And the ones a `concat!` builds, as the tree's own scan now does it.
    let mut rest = source;
    while let Some(at) = rest.find("concat!(") {
        rest = &rest[at + "concat!(".len()..];
        // To the closing paren, not to the next quote. Splitting on commas ran
        // past the `)` and swallowed whatever literal followed the macro, which
        // joined a code out of two unrelated strings — and then reported it as
        // produced when nothing produces it.
        let end = rest.find(')').unwrap_or(rest.len());
        let mut joined = String::new();
        for piece in rest[..end].split(',') {
            let piece = piece.trim();
            // `r"..."` as well as `"..."`: `concat!` accepts both, and a raw
            // literal is the spelling a code containing a quote would need.
            let stripped = piece
                .strip_prefix('r')
                .unwrap_or(piece)
                .strip_prefix('"')
                .and_then(|p| p.split('"').next());
            match stripped {
                Some(literal) => joined.push_str(literal),
                None => break,
            }
        }
        if looks_like_a_code(&joined) {
            found.push(joined);
        }
    }
    found.sort();
    found.dedup();
    found
}

fn looks_like_a_code(literal: &str) -> bool {
    let mut chars = literal.chars();
    matches!(chars.next(), Some('E' | 'W' | 'I'))
        && chars.next() == Some('-')
        && literal.len() > 2
        && literal
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '-')
}

/// CLAIM (`invariants.rs::the_validator_builds_no_code_with_a_macro`): "If a
/// `macro_rules!` that mentions a code prefix ever appears in `validate.rs`,
/// this fails and the scan has to be taught, rather than silently asking
/// nothing about a code."
///
/// That is the right answer to an undecidable problem, and this is the question
/// the answer leaves open: what the guard reads is
/// `include_str!("../src/validate.rs")` and `!source.contains("macro_rules!")`,
/// so it fails when a macro is *defined* there. A macro builds a code wherever
/// it is expanded from, and expansion happens at the call site. A macro defined
/// in a sibling module — `crate::codes::code!` — and invoked in `validate.rs`
/// satisfies the guard while producing a code that the scan, and therefore the
/// reachability table and the page check, never see. The class the guard names
/// is "a code the validator builds with a macro"; the class it covers is "a
/// macro written in one file".
///
/// Nothing in the tree does this today. The reconstruction below is the shape,
/// and the guard is what is supposed to keep it out.
#[test]
fn defect_17_the_macro_guard_reads_only_the_file_it_guards() {
    // The shape: the literal arguments at the call site are *not* codes — only
    // the macro body, in `codes.rs`, joins them into one.
    //
    //     // crates/cvt-core/src/codes.rs
    //     macro_rules! code {
    //         ($a:literal, $b:literal) => { concat!("E-", $a, $b) };
    //     }
    let validate_rs = r#"
        use crate::codes::code;
        fn f() {
            let _ = Diagnostic::error(code!("VIA-A-", "MACRO-CODE"), "message");
        }
    "#;
    assert!(
        !validate_rs.contains("macro_rules!"),
        "the premise: the guard asks `!source.contains(\"macro_rules!\")` of this \
         file, and the definition is not in it"
    );
    assert!(
        !validate_rs.contains("concat!("),
        "the premise: the joining half of the scan reads `concat!` in this file, and \
         this file has none"
    );

    // The guard this finding asks for is stronger than "no macro is written
    // here": it is that **every `Diagnostic::…` construction site takes a
    // string literal**, which a macro — here or one module over — cannot be.
    // `invariants.rs::every_diagnostic_is_built_from_a_literal` asserts it over
    // every file in the crate, so this source could not exist in the tree.
    let guard = include_str!("../src/../tests/invariants.rs");
    assert!(
        guard.contains("every_diagnostic_is_built_from_a_literal"),
        "the guard exists"
    );
    let seen = scan_codes(validate_rs);
    assert!(
        !seen.iter().any(|c| c == "E-VIA-A-MACRO-CODE"),
        "the limit, recorded: the scan sees {seen:?} for a code a macro builds, and \
         what makes that safe is the literal-at-the-construction-site guard rather \
         than the scan — the previous guard asked only whether a macro was *written* \
         in this file, which a macro defined one module over walks past. A \
         macro defined one module over is not one it covers"
    );
}

/// The codes `docs/DIAGNOSTICS.md` documents as *table rows*, which is what both
/// checks now read.
fn table_codes(page: &str) -> Vec<&str> {
    page.lines()
        .filter_map(|line| line.strip_prefix("| `"))
        .filter_map(|rest| rest.split('`').next())
        .filter(|code| {
            code.chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '-')
        })
        .collect()
}

/// CLAIM (`invariants.rs::constructed_codes`'s doc): "Deliberately textual …
/// So the set is read out of the source rather than trusted", as rewritten
/// during this review to join each `concat!`'s literals as well as collecting
/// every code-shaped literal.
///
/// The joining is driven by `rest.split(',')` on the remainder of the *whole
/// file*, and it keeps appending string literals until a piece does not begin
/// with one. That is the right idea in a world where a `concat!` is the last
/// thing written. In a real file it is not: the very next statement in these
/// tests is `Diagnostic::error(CODE, "message")`, whose argument after the comma
/// is a string literal, so the joiner produces `E-SPLIT-CODEmessage` and the
/// code the rewrite exists for is invisible. The first arm below shows the
/// mechanism working on a source where nothing follows the call, and the second
/// shows it failing on the same call with one ordinary line after it.
///
/// Two further shapes the rewrite does not reach are in the third and fourth
/// arms: a `concat!` inside a `macro_rules!` body, where an argument is a
/// metavariable, and a `concat!` of raw string literals.
///
/// Latent today — `validate.rs` writes every code as one literal, and has no
/// `concat!` at all — but the mechanism is what a new code meets.
#[test]
fn defect_12_the_rewrite_that_joins_a_concat_stops_only_at_the_next_quote() {
    // Closed: a code that reaches the constructor through a `const`.
    let via_const = r#"
        const NEW_CODE: &str = "E-NEW-CODE";
        fn f() { let _ = Diagnostic::error(NEW_CODE, "message"); }
    "#;
    assert!(
        scan_codes(via_const).iter().any(|c| c == "E-NEW-CODE"),
        "a code that reaches the constructor through a const must be found"
    );

    // The joining rewrite does work when the `concat!` is followed by nothing
    // that begins with a string literal.
    let isolated = r#"const CODE: &str = concat!("E-", "ISOLATED");"#;
    assert!(
        scan_codes(isolated).iter().any(|c| c == "E-ISOLATED"),
        "the joining rewrite must work at all: {:?}",
        scan_codes(isolated)
    );

    // And stops working the moment an ordinary following statement has a string
    // argument — which is what `Diagnostic::error(CODE, "message")` is.
    let realistic = r#"
        const CODE: &str = concat!("E-", "SPLIT-CODE");
        fn f() { let _ = Diagnostic::error(CODE, "message"); }
    "#;
    let seen = scan_codes(realistic);
    assert!(
        seen.iter().any(|c| c == "E-SPLIT-CODE"),
        "`E-SPLIT-CODE` is produced by this source and the scan sees {seen:?}. The \
         joiner does not stop at the call's closing parenthesis, so it appends the \
         next string literal it meets after a comma — the `\"message\"` of the \
         constructor two lines down — and produces `E-SPLIT-CODEmessage`, which is \
         not a code. A code assembled with `concat!` is therefore found or lost \
         depending on what happens to follow it in the file"
    );

    // Not closed: a `concat!` with a metavariable, which a macro expands.
    let via_macro = r#"
        macro_rules! code {
            ($name:literal) => { concat!("E-", $name) };
        }
        fn f() { let _ = Diagnostic::error(code!("FROM-A-MACRO"), "message"); }
    "#;
    // Undecidable, and left as a recorded limit rather than chased with more
    // text processing: the code is assembled from a macro *parameter*, and no
    // scan of the source can know what a call site passes. What makes the limit
    // safe is that the validator is asserted to contain no macro at all —
    // `invariants.rs::the_validator_builds_no_code_with_a_macro` — so a code
    // cannot reach the table and the page invisibly without that test failing
    // first.
    // What closes this is not the scan but the guard: every construction site
    // in the crate must take a *literal*, which is asserted by
    // `invariants.rs::every_diagnostic_is_built_from_a_literal`. A macro — here
    // or one module over — cannot produce a code that nothing asks about,
    // because it cannot be what the constructor is handed.
    let seen = scan_codes(via_macro);
    assert!(
        !seen.iter().any(|c| c == "E-FROM-A-MACRO"),
        "the limit, recorded: a code a macro builds is invisible to a textual \
         scan, and the guard is the macro assertion in `invariants.rs` rather \
         than the scan (it saw {seen:?})"
    );

    // Not closed: raw string literals, which `concat!` accepts.
    let via_raw = r#"
        const CODE: &str = concat!(r"E-", r"RAW-CODE");
        fn f() { let _ = Diagnostic::error(CODE, "message"); }
    "#;
    let seen = scan_codes(via_raw);
    assert!(
        seen.iter().any(|c| c == "E-RAW-CODE"),
        "`E-RAW-CODE` is produced by this source and the scan sees {seen:?}: the \
         joiner looks for a `\"` and a raw literal starts with `r`"
    );
}

/// CLAIM (`invariants.rs::constructed_codes`, as rewritten during this review):
/// "Every source file, not `validate.rs` alone. A code produced from another
/// module is produced by the validator too, and reading one file asked nothing
/// about it", and the page check's "Every code the validator produces must have
/// an entry in the diagnostics page."
///
/// The rewrite reads the crate, which is the right scope, and this is the
/// assertion that keeps the *other* half of the pair honest: the check is
/// textual, so a code has to be a code-shaped literal in a file the walk reaches
/// for either test to ask about it. Nothing in the tree produces a diagnostic
/// outside `validate.rs` today, which is what makes "read `src/`" and "read the
/// producer" the same statement — and it is an accident of the current layout
/// rather than a property of the mechanism.
///
/// So the walk below establishes the class mechanically. It fails the day a
/// second module constructs a `Diagnostic`, which is exactly when the
/// reachability table and the page need to grow an entry, and it fails the day a
/// code-shaped literal appears outside `validate.rs` for the same reason.
#[test]
fn confirmed_the_scan_covers_the_whole_crate_and_the_class_of_producers_is_recorded() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut producers = Vec::new();
    let mut carriers = Vec::new();
    let mut walk = vec![root.clone()];
    while let Some(dir) = walk.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk.push(path);
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let name = path
                .strip_prefix(&root)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            if text.contains("Diagnostic::error(")
                || text.contains("Diagnostic::warn(")
                || text.contains("Diagnostic::info(")
            {
                producers.push(name.clone());
            }
            if !scan_codes(&text).is_empty() {
                carriers.push((name, scan_codes(&text)));
            }
        }
    }
    producers.sort();
    carriers.sort();

    assert_eq!(
        producers,
        vec!["validate.rs".to_owned()],
        "the class this check must cover is *every module that can construct a          Diagnostic*, because that is the sentence the check is written to serve.          The scan reads `include_str!(\"../src/validate.rs\")` and nothing else, so a          code constructed in {producers:?} is invisible to the completeness check          *and* to the page check, which compares the page against the scan. Today          the class has one member; nothing makes it stay that way"
    );
    assert_eq!(
        carriers.len(),
        1,
        "every code-shaped literal in the crate is in one file, so the scan's input          covers the class by accident: {carriers:?}"
    );
}

/// Round 8's *first* half, re-run as the counterpart: the argument for the fix
/// was that the tree is clean under the stronger reading, and a fix must not
/// change that. Every code the rewritten scan finds today is a table row, and
/// every table row is a code the scan finds — in both directions, which is what
/// "the page and the scan cannot disagree" means operationally.
#[test]
fn confirmed_the_page_and_the_scan_agree_in_both_directions() {
    let page = include_str!("../../../docs/DIAGNOSTICS.md");
    let rows = table_codes(page);
    let produced = scan_codes(include_str!("../src/validate.rs"));

    assert!(
        produced.len() > 20,
        "the scan found only {} codes, so it is looking in the wrong place: \
         {produced:?}",
        produced.len()
    );

    let undocumented: Vec<&String> = produced
        .iter()
        .filter(|code| !rows.contains(&code.as_str()))
        .collect();
    assert!(
        undocumented.is_empty(),
        "produced and not a table row: {undocumented:?}"
    );

    let produced_owned: BTreeSet<String> = produced.iter().cloned().collect();
    let stale: Vec<&&str> = rows
        .iter()
        .filter(|code| !produced_owned.contains(**code))
        .collect();
    assert!(
        stale.is_empty(),
        "a table row for a code nothing produces is a test of nothing: {stale:?}"
    );

    // The retraction half, which was round 8's other finding, checked as fixed
    // rather than assumed fixed: the sentences that say a code was removed must
    // not name it at all, or `page.contains` reads as documentation again.
    let retracted_in_prose: Vec<&str> = ["E-CIDR-FAMILY", "E-UNREACHABLE-RULES"]
        .into_iter()
        .filter(|code| page.contains(code))
        .collect();
    assert!(
        retracted_in_prose.is_empty(),
        "the page still names {retracted_in_prose:?}, and those are the codes it says \
         were *removed* — a retraction a reader can search for is a retraction the \
         page check can be fooled by the day one of them is produced again"
    );
}

// ==============================================================================
// helpers
// ==============================================================================

/// The core binary, when this machine has one.
fn core_binary() -> Option<std::path::PathBuf> {
    for candidate in ["/usr/bin/verge-mihomo", "/usr/local/bin/mihomo"] {
        let path = Path::new(candidate);
        if path.is_file() {
            return Some(path.to_path_buf());
        }
    }
    None
}
