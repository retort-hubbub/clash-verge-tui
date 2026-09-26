//! A **fifth**, independent pass over `cvt-core`, aimed at the two things no
//! reviewer has seen: the selection memory (`d91366b`, 0.5.0) and the fixes in
//! `810e28a` (0.4.1).
//!
//! Rules this file follows:
//!
//! * the newest code is attacked first — `Service::restore_selections`, written
//!   by the author and reviewed by nobody;
//! * a claim about the core is settled by the core: `/usr/bin/verge-mihomo`
//!   `v1.19.31`, driven with `-t` (which exits on its own) and — for the
//!   selection claims — started and probed inside a single test;
//! * `defect_*` names preserve the original findings; all tests now must pass;
//! * generated cases where the claim is behavioural, hand-picked only where the
//!   defect is a specific shape.
//!
//! # The live evidence behind the selection findings
//!
//! Measured against a real `v1.19.31` core (`GET /group/{name}` polled after
//! `PUT /proxies/grp-urltest {"name":"node-b"}` returning `204`):
//!
//! ```text
//! +0s      now='node-a' fixed='node-b'
//! +1s      now='node-a' fixed='node-b'
//! +3s      now='node-a' fixed='node-b'
//! +5s      now='DIRECT' fixed='node-b'
//! ```
//!
//! For a `Selector`, `GET /group/{name}` reports the pin in `now`. For a
//! `URLTest`/`Fallback` the pin is `fixed` and `now` is the node the group is
//! *currently using*, which the group's own background testing keeps changing.
//! `restore_selections` confirms a replay by reading `now`, so it reads the
//! wrong field for two of the three group types the core lets a client select
//! in (`PUT /proxies/{name}` answers `204` for `Selector`, `URLTest` and
//! `Fallback`, and `400 {"message":"Must be a Selector"}` for `LoadBalance`).
//!
//! The fake controller below is a faithful model of that: it implements the
//! three semantics the live probes established (select sets `now` on a
//! `Selector` and `fixed` on a `URLTest`, `GET /group/` with an empty name
//! answers the *list*, a `LoadBalance` refuses a select), so the tests are
//! deterministic without needing a core per assertion.
//!
//! Nothing here modifies a source file.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::too_many_lines
)]

use std::collections::BTreeMap;
use std::path::Path as StdPath;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cvt_core::AppPaths;
use cvt_core::mihomo::supervisor::Supervisor;
use cvt_core::model::config::Config;
use cvt_core::profile::item::{PrfItem, ProfileType};
use cvt_core::profile::store::{ProfileStore, document_path};
use cvt_core::{Service, validate};
use tempfile::TempDir;

// ------------------------------------------------------------------ helpers

/// The core this project validates against; `-t` exits on its own.
const CORE: &str = "/usr/bin/verge-mihomo";

fn core_available() -> bool {
    StdPath::new(CORE).is_file()
}

fn home() -> (TempDir, AppPaths) {
    let dir = TempDir::new().unwrap();
    let paths = AppPaths::new(dir.path());
    paths.ensure_dirs().unwrap();
    (dir, paths)
}

/// A home whose `profiles.yaml` is written out verbatim, plus documents.
///
/// The index is written by hand rather than through `ProfileStore::add`
/// because half of these tests are about an index this program did **not**
/// write — an imported one.
fn home_with_index(index: &str, documents: &[(&str, &str)]) -> (TempDir, AppPaths) {
    let (dir, paths) = home();
    std::fs::write(paths.profiles_index(), index).unwrap();
    for (name, body) in documents {
        std::fs::write(paths.profiles_dir().join(name), body).unwrap();
    }
    (dir, paths)
}

fn errors(config: &Config) -> Vec<&'static str> {
    validate::check(config)
        .errors_iter()
        .map(|d| d.code)
        .collect()
}

/// Ask the real core whether it will load a document. The verdict is
/// `test is successful`; the log lines go to stdout too.
fn core_accepts(text: &str) -> bool {
    assert!(
        core_available(),
        "{CORE} is required for the checks that settle a claim against the core"
    );
    let dir = TempDir::new().unwrap();
    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let path = dir.path().join("config.yaml");
    std::fs::write(&path, text).unwrap();
    let out = std::process::Command::new(CORE)
        .arg("-t")
        .arg("-d")
        .arg(&work)
        .arg("-f")
        .arg(&path)
        .output()
        .unwrap();
    let verdict = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let accepted = verdict.contains("test is successful");
    if !accepted {
        println!("--- {CORE} -t rejected:\n{verdict}");
    }
    accepted
}

// --------------------------------------------------- the fake control plane

/// The state a mihomo controller keeps for its groups.
#[derive(Default)]
struct PanelState {
    /// group name -> `select` | `url-test` | `fallback` | `load-balance`
    groups: Vec<(String, String)>,
    /// The reported `now`: what the group is *using*.
    now: BTreeMap<String, String>,
    /// The reported `fixed`: the pin, on `url-test`/`fallback` only.
    fixed: BTreeMap<String, String>,
    /// Groups that answer `404` for this many more `GET /group/{name}` calls,
    /// which is exactly what the core does while a reload is in flight.
    absent: BTreeMap<String, usize>,
    members: BTreeMap<String, Vec<String>>,
    /// A core that answers reads and never answers a select — which is what a
    /// core busy applying a new configuration does for a moment.
    hangs_on_select: bool,
}

/// A controller that answers the three `/group` and `/proxies` shapes the live
/// probes recorded.
struct Panel {
    port: u16,
    seen: Arc<Mutex<Vec<String>>>,
    state: Arc<Mutex<PanelState>>,
}

impl Panel {
    fn endpoint(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }

    fn requests(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }

    fn saw(&self, needle: &str) -> bool {
        self.requests().iter().any(|r| r.contains(needle))
    }

    fn hits(&self, needle: &str) -> usize {
        self.requests()
            .iter()
            .filter(|r| r.contains(needle))
            .count()
    }
}

fn group_json(state: &PanelState, name: &str, kind: &str) -> String {
    let members = state.members.get(name).cloned().unwrap_or_default();
    let all: String = members
        .iter()
        .map(|m| format!("{m:?}"))
        .collect::<Vec<_>>()
        .join(",");
    let mut fields = vec![
        format!("\"name\":{name:?}"),
        format!("\"type\":{kind:?}"),
        format!("\"all\":[{all}]"),
        "\"history\":[]".to_owned(),
    ];
    match kind {
        "load-balance" => {
            // Verified live: a `LoadBalance` group reports no `now` at all.
        }
        "url-test" | "fallback" => {
            fields.push(format!(
                "\"now\":{:?}",
                state.now.get(name).cloned().unwrap_or_default()
            ));
            fields.push(format!(
                "\"fixed\":{:?}",
                state.fixed.get(name).cloned().unwrap_or_default()
            ));
        }
        _ => fields.push(format!(
            "\"now\":{:?}",
            state.now.get(name).cloned().unwrap_or_default()
        )),
    }
    format!("{{{}}}", fields.join(","))
}

/// What the core answers, per the live probes.
fn respond(state: &mut PanelState, method: &str, target: &str, member: &str) -> (u16, String) {
    let path = target.split('?').next().unwrap_or(target);
    let not_found = (404, "{\"message\":\"Resource not found\"}".to_owned());
    if let Some(name) = path.strip_prefix("/group/") {
        if name.is_empty() {
            // Verified live: `GET /group/` answers the *group list*, not a
            // group — the client is asking for the group named "".
            let groups: Vec<String> = state
                .groups
                .iter()
                .map(|(n, k)| group_json(state, n, k))
                .collect();
            return (200, format!("{{\"proxies\":[{}]}}", groups.join(",")));
        }
        let Some((_, kind)) = state.groups.iter().find(|(n, _)| n == name) else {
            return not_found;
        };
        let kind = kind.clone();
        let remaining = state.absent.get(name).copied().unwrap_or(0);
        if remaining > 0 {
            state.absent.insert(name.to_owned(), remaining - 1);
            return not_found;
        }
        return (200, group_json(state, name, &kind));
    }
    if let Some(name) = path.strip_prefix("/proxies/") {
        if name.is_empty() {
            // Verified live: `PUT /proxies/` (no name) answers 405.
            return (405, String::new());
        }
        let Some((_, kind)) = state.groups.iter().find(|(n, _)| n == name) else {
            return not_found;
        };
        if method != "PUT" {
            return (405, String::new());
        }
        if !state
            .members
            .get(name)
            .is_some_and(|m| m.iter().any(|x| x == member))
        {
            return (
                400,
                "{\"message\":\"Selector update error: proxy not exist\"}".to_owned(),
            );
        }
        match kind.as_str() {
            "load-balance" => (400, "{\"message\":\"Must be a Selector\"}".to_owned()),
            // Verified live: a select on a url-test sets `fixed`, and `now`
            // keeps reporting the node the group is using.
            "url-test" | "fallback" => {
                state.fixed.insert(name.to_owned(), member.to_owned());
                (204, String::new())
            }
            _ => {
                state.now.insert(name.to_owned(), member.to_owned());
                (204, String::new())
            }
        }
    } else {
        not_found
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Error",
    }
}

fn wire(status: u16, body: &str) -> String {
    format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        reason(status),
        body.len()
    )
}

/// Start a fake controller holding the named groups, in the given order.
fn core_with(groups: &[(&str, &str)]) -> Panel {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let mut state = PanelState::default();
    for (name, kind) in groups {
        state.groups.push(((*name).to_owned(), (*kind).to_owned()));
        state.members.insert(
            (*name).to_owned(),
            vec![
                "node-a".to_owned(),
                "node-b".to_owned(),
                "DIRECT".to_owned(),
            ],
        );
        state.now.insert((*name).to_owned(), "node-a".to_owned());
    }
    let state = Arc::new(Mutex::new(state));
    let shared = Arc::clone(&state);
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let listener = tokio::net::TcpListener::from_std(listener).unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let state = Arc::clone(&shared);
            let sink = Arc::clone(&sink);
            tokio::spawn(async move {
                let mut buf: Vec<u8> = Vec::new();
                let mut chunk = [0u8; 1024];
                let head_end = loop {
                    match socket.read(&mut chunk).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => {
                            buf.extend_from_slice(&chunk[..n]);
                            if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                                break at + 4;
                            }
                        }
                    }
                };
                let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
                let length: usize = head
                    .lines()
                    .find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        k.eq_ignore_ascii_case("content-length")
                            .then(|| v.trim().parse().ok())?
                    })
                    .unwrap_or(0);
                while buf.len() < head_end + length {
                    match socket.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                }
                let body = String::from_utf8_lossy(&buf[head_end..]).into_owned();
                let mut lines = head.lines();
                let request_line = lines.next().unwrap_or_default().to_owned();
                let method = request_line
                    .split(' ')
                    .next()
                    .unwrap_or_default()
                    .to_owned();
                let target = request_line
                    .split(' ')
                    .nth(1)
                    .unwrap_or_default()
                    .to_owned();
                let member = serde_json::from_str::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|v| v.get("name").and_then(|n| n.as_str()).map(str::to_owned))
                    .unwrap_or_default();
                sink.lock().unwrap().push(format!("{method} {target}"));
                if state.lock().unwrap().hangs_on_select && method == "PUT" {
                    // Accept the connection, read the request, never answer: the
                    // client's own timeout is the only thing that ends this.
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    return;
                }
                let (status, payload) =
                    respond(&mut state.lock().unwrap(), &method, &target, &member);
                let _ = socket.write_all(wire(status, &payload).as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    Panel { port, seen, state }
}

// --------------------------------------------------------------- the index

/// `profiles.yaml` in the shape this project writes, with `selected` supplied
/// verbatim so an imported/foreign shape can be tested.
fn index_with(selected: &str) -> String {
    format!(
        "current: L1\nitems:\n  - uid: L1\n    type: local\n    name: base\n    file: L1.yaml\n    selected:\n{selected}"
    )
}

fn base_document(endpoint: &str) -> String {
    format!(
        "mixed-port: 7890\nexternal-controller: {endpoint}\nmode: rule\nproxies:\n  - {{name: node-a, type: socks5, server: 127.0.0.1, port: 1080}}\n  - {{name: node-b, type: socks5, server: 127.0.0.1, port: 1081}}\nproxy-groups:\n  - {{name: grp-select, type: select, proxies: [node-a, node-b, DIRECT]}}\nrules:\n  - MATCH,grp-select\n"
    )
}

/// A service whose hand-written index remembers `selected`.
fn service_with_selected(endpoint: &str, selected: &str) -> (TempDir, Service) {
    let (dir, paths) = home_with_index(
        &index_with(selected),
        &[("L1.yaml", &base_document(endpoint))],
    );
    let service = Service::open(paths).unwrap();
    (dir, service)
}

// ======================================================= 1. selection memory

/// CLAIM (commit `d91366b`): "The replay now waits for the group to answer and
/// confirms the choice took."
///
/// `confirm_selection` decides the choice took by reading `view.now`. Live, on
/// a real core, `PUT /proxies/{url-test group}` answers `204`, the group's
/// `fixed` becomes the member, and `now` keeps reporting the node the group is
/// currently using — measured `now='node-a' fixed='node-b'` for three seconds
/// after a successful select, then `now='DIRECT'`.
///
/// So for a `URLTest`/`Fallback` — two of the three group types the core lets a
/// client select in — the confirmation reads the wrong field and the choice
/// that *did* take is reported as one that did not.
#[tokio::test]
async fn defect_1_a_url_test_pin_that_took_is_reported_as_not_restored() {
    let panel = core_with(&[("grp-urltest", "url-test")]);
    let (_dir, service) = service_with_selected(
        &panel.endpoint(),
        "      - name: grp-urltest\n        now: node-b\n",
    );

    let applied = service.restore_selections().await.unwrap();

    assert_eq!(
        applied,
        1,
        "the pin was applied (`PUT {}` answered 204 and the group reports \
         fixed=\"node-b\", exactly as the live core does) but the replay counted \
         {applied}. Requests the panel saw: {:?}",
        "/proxies/grp-urltest",
        panel.requests()
    );
}

/// CLAIM (commit `d91366b`): "The whole replay shares one now, so a profile
/// with twenty remembered groups and a core that has lost them all costs a
/// second and a half rather than half a minute."
///
/// The deadline is shared, but it is also **spent**: the first group whose
/// confirmation does not match burns the entire budget, and the loop then
/// `break`s before the second group is looked at. One group the subscription
/// no longer has — the documented, expected case — silently costs every later
/// group its replay, although the doc comment promises the opposite ("a group
/// the subscription has renamed … is skipped rather than failing the others").
#[tokio::test]
async fn defect_2_one_dead_group_costs_every_later_group_its_replay() {
    let panel = core_with(&[("grp-urltest", "url-test"), ("grp-select", "select")]);
    let (_dir, service) = service_with_selected(
        &panel.endpoint(),
        "      - name: grp-urltest\n        now: node-b\n      - name: grp-select\n        now: node-b\n",
    );

    let applied = service.restore_selections().await.unwrap();

    assert!(
        panel.saw("PUT /proxies/grp-select"),
        "the second remembered group was never replayed at all: the first \
         group's confirmation consumed the shared deadline and the loop broke. \
         Decisions restored: {applied}. Requests the panel saw: {:?}",
        panel.requests()
    );
}

/// The same starvation, reached through the shape the commit message itself
/// calls the reason the confirmation exists: a reload is applied in the
/// background and a group is briefly absent.
///
/// A group the *new* document dropped is absent for good. `wait_for_group`
/// then spends the whole shared deadline on it, and every group after it is
/// skipped — including ones that are present and selectable.
#[tokio::test]
async fn defect_3_a_group_that_never_comes_back_starves_the_replay() {
    let panel = core_with(&[("grp-urltest", "url-test"), ("grp-select", "select")]);
    // The first remembered group is gone: 404 forever.
    panel
        .state
        .lock()
        .unwrap()
        .absent
        .insert("grp-urltest".to_owned(), usize::MAX);
    let (_dir, service) = service_with_selected(
        &panel.endpoint(),
        "      - name: grp-urltest\n        now: node-b\n      - name: grp-select\n        now: node-b\n",
    );

    let started = Instant::now();
    let applied = service.restore_selections().await.unwrap();
    let elapsed = started.elapsed();

    assert!(
        panel.saw("PUT /proxies/grp-select"),
        "`grp-select` is present and selectable, and the documentation says a \
         group that no longer exists is `skipped rather than failing the \
         others` — but the missing first group spent the shared deadline \
         ({elapsed:?} elapsed, {applied} restored). Requests: {:?}",
        panel.requests()
    );
}

/// An imported index whose `selected` list contains an empty entry — which the
/// reference project's frontend tolerates (`each.name != null && each.now !=
/// null` filters them) — names a group called `""`.
///
/// `GET /group/` is what the core answers for that name: verified live, it
/// returns **200 and the group list**, whose body has no `name` field, so the
/// client refuses to decode it and `wait_for_group("")` spins to the deadline.
/// One junk entry therefore costs every real choice its replay.
#[tokio::test]
async fn defect_4_an_empty_entry_in_an_imported_index_starves_the_replay() {
    let panel = core_with(&[("grp-select", "select")]);
    let (_dir, service) = service_with_selected(
        &panel.endpoint(),
        "      - name: \"\"\n        now: node-a\n      - name: grp-select\n        now: node-b\n",
    );

    let applied = service.restore_selections().await.unwrap();

    assert!(
        panel.saw("PUT /proxies/grp-select"),
        "an empty group name in the index stopped the real choice from being \
         replayed ({applied} restored). Requests: {:?}",
        panel.requests()
    );
}

/// CLAIM (commit `d91366b`): "The choice is recorded in the index (`selected`,
/// which is also the key the reference project uses, so an imported index
/// round-trips)."
///
/// The reference's entry type is `PrfSelected { name: Option<String>, now:
/// Option<String> }` — *both* optional and, since neither field carries
/// `skip_serializing_if`, both written even when absent:
///
/// ```ignore
/// #[derive(Default, Debug, Clone, Deserialize, Serialize)]
/// pub struct PrfSelected {
///     pub name: Option<String>,
///     pub now: Option<String>,
/// }
/// ```
///
/// (clash-verge-rev `src-tauri/src/config/prfitem.rs`.) So `- name: null` and an
/// entry with no `now:` are both shapes the reference writes and reads. This
/// project's `SelectedNode` requires a `String` for both, which makes
/// `ProfileStore::load` fail — and the index is the user's whole profile list,
/// so a single such entry makes every command refuse to run.
#[test]
fn defect_5_a_foreign_selected_entry_with_an_optional_field_makes_the_index_unreadable() {
    for (why, entry) in [
        (
            "`now` absent (the reference writes `now: null`)",
            "      - name: grp-select\n",
        ),
        (
            "`now` null (the reference's field is `Option<String>`)",
            "      - name: grp-select\n        now: null\n",
        ),
        (
            "`name` null (the reference's field is `Option<String>`)",
            "      - name: null\n        now: node-b\n",
        ),
    ] {
        let (_dir, paths) = home_with_index(
            &index_with(entry),
            &[("L1.yaml", "mode: rule\nrules:\n  - MATCH,DIRECT\n")],
        );
        let loaded = ProfileStore::load(&paths);
        assert!(
            loaded.is_ok(),
            "an index the reference project round-trips was refused because of \
             {why}: {}\n--- index ---\n{}\n",
            loaded.err().map(|e| e.to_string()).unwrap_or_default(),
            index_with(entry)
        );
    }
}

/// The author's own claim, checked rather than assumed: a group that is briefly
/// absent while a reload is applied is waited for, and the choice is replayed.
#[tokio::test]
async fn a_group_that_reappears_within_the_budget_is_replayed() {
    let panel = core_with(&[("grp-select", "select")]);
    // The group answers 404 for the first three reads — 150 ms at the replay's
    // 50 ms step — which is the window the commit message describes.
    panel
        .state
        .lock()
        .unwrap()
        .absent
        .insert("grp-select".to_owned(), 3);
    let (_dir, service) = service_with_selected(
        &panel.endpoint(),
        "      - name: grp-select\n        now: node-b\n",
    );

    assert_eq!(service.restore_selections().await.unwrap(), 1);
    assert_eq!(
        panel.state.lock().unwrap().now.get("grp-select"),
        Some(&"node-b".to_owned())
    );
}

/// A `Selector` group — where `now` *is* the pin — is replayed and confirmed.
#[tokio::test]
async fn a_selector_group_is_replayed_and_confirmed() {
    let panel = core_with(&[("grp-select", "select")]);
    let (_dir, service) = service_with_selected(
        &panel.endpoint(),
        "      - name: grp-select\n        now: node-b\n",
    );

    assert_eq!(service.restore_selections().await.unwrap(), 1);
    assert_eq!(panel.hits("PUT /proxies/grp-select"), 1);
}

/// A `LoadBalance` group cannot be selected in (`PUT` answers `400 Must be a
/// Selector`), and it reports no `now` at all, so it must be skipped without
/// burning the budget — and the groups *after* it must still be replayed.
#[tokio::test]
async fn a_load_balance_entry_does_not_starve_the_groups_after_it() {
    let panel = core_with(&[("grp-lb", "load-balance"), ("grp-select", "select")]);
    let (_dir, service) = service_with_selected(
        &panel.endpoint(),
        "      - name: grp-lb\n        now: node-b\n      - name: grp-select\n        now: node-b\n",
    );

    let applied = service.restore_selections().await.unwrap();
    assert!(
        panel.saw("PUT /proxies/grp-select"),
        "a load-balance entry (a group type the reference stores selections \
         for, and that the core refuses to select in) stopped the selectable \
         group after it from being replayed. Requests: {:?}",
        panel.requests()
    );
    assert_eq!(applied, 1, "only the selector can be restored");
}

/// The choices are per *group*: a second choice in the same group replaces the
/// first, and unpinning forgets it. Checked through the index rather than the
/// store's in-memory list, because the index is what survives an apply.
#[tokio::test]
async fn the_last_choice_for_a_group_wins_and_unpin_forgets_it() {
    let (_dir, service) = service_with_selected(
        "127.0.0.1:1",
        "      - name: grp-select\n        now: node-a\n",
    );
    service.remember_selection("grp-select", "node-b").unwrap();
    assert_eq!(service.store().unwrap().selections().len(), 1);
    assert_eq!(service.store().unwrap().selections()[0].now, "node-b");

    service.forget_selection("grp-select").unwrap();
    assert!(service.store().unwrap().selections().is_empty());
}

/// A duplicated entry — two entries for one group, which only an imported index
/// can produce, because `amend_selection` replaces rather than appends. The
/// last one has to win, since `selected` is documented as "newest last".
#[tokio::test]
async fn duplicate_entries_in_an_imported_index_end_on_the_last_one() {
    let panel = core_with(&[("grp-select", "select")]);
    let (_dir, service) = service_with_selected(
        &panel.endpoint(),
        "      - name: grp-select\n        now: node-a\n      - name: grp-select\n        now: node-b\n",
    );

    service.restore_selections().await.unwrap();
    assert_eq!(
        panel.state.lock().unwrap().now.get("grp-select"),
        Some(&"node-b".to_owned()),
        "the newest entry must be the one the core ends up on"
    );
}

/// CLAIM: "the choice is recorded in the index … written by `proxies
/// select`/`unpin`" — so the value a *user* would see in `profiles.yaml` has to
/// survive a round trip, key names included.
#[test]
fn a_selected_entry_round_trips_through_the_index() {
    let (_dir, paths) = home_with_index(
        &index_with("      - name: grp-select\n        now: \"node b/slash\"\n"),
        &[("L1.yaml", "mode: rule\nrules:\n  - MATCH,DIRECT\n")],
    );
    let store = ProfileStore::load(&paths).unwrap();
    let selections = store.selections();
    assert_eq!(selections.len(), 1);
    assert_eq!(selections[0].name, "grp-select");
    assert_eq!(selections[0].now, "node b/slash");

    store.save().unwrap();
    let text = std::fs::read_to_string(paths.profiles_index()).unwrap();
    assert!(text.contains("selected:"), "{text}");
    let reloaded = ProfileStore::load(&paths).unwrap();
    assert_eq!(reloaded.selections(), selections);
}

/// The index write happens *after* the core accepted the selection, so a
/// failure there is the one moment where the core and the index disagree. The
/// claim is that the index is written atomically; the check is that a failed
/// write leaves the file exactly as it was, and that the failure is reported
/// rather than swallowed.
#[test]
fn a_failed_index_write_reports_and_changes_nothing() {
    use std::os::unix::fs::PermissionsExt as _;

    let (_dir, service) = service_with_selected(
        "127.0.0.1:1",
        "      - name: grp-select\n        now: node-a\n",
    );
    let path = service.paths().profiles_index();
    let before = std::fs::read_to_string(&path).unwrap();

    // A home that cannot be written to: the temporary file cannot be created.
    let home = service.paths().home().to_path_buf();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o500)).unwrap();
    let result = service.remember_selection("grp-select", "node-b");
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();

    assert!(
        result.is_err(),
        "an unwritable index must not report success"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        before,
        "the index must be untouched by a failed write"
    );
    assert_eq!(
        service.store().unwrap().selections()[0].now,
        "node-a",
        "and nothing may have been recorded"
    );
}

/// The replay's own bound. The commit message promises that "the whole wait is
/// bounded so a core that is reloading cannot make an apply hang".
///
/// A request to a core that accepts the connection but never answers must be
/// bounded by the replay's overall budget, not the client's ten-second timeout.
#[tokio::test]
async fn the_replay_stays_within_its_deadline_when_the_controller_hangs() {
    use tokio::io::AsyncReadExt as _;

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let listener = tokio::net::TcpListener::from_std(listener).unwrap();
    // A black hole: accept, read, never answer.
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut chunk = [0u8; 1024];
                let _ = socket.read(&mut chunk).await;
                tokio::time::sleep(Duration::from_secs(60)).await;
            });
        }
    });

    let (_dir, service) = service_with_selected(
        &format!("127.0.0.1:{port}"),
        "      - name: grp-select\n        now: node-b\n",
    );

    let started = Instant::now();
    let applied = service.restore_selections().await.unwrap();
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_millis(2500),
        "the replay took {elapsed:?} against a core that answers nothing, for a \
         budget of seconds at most ({applied} restored); no read is bounded by \
         request that is already in flight"
    );
}

// ============================== 2. index-derived names, generated rather than
//                                hand-picked

/// CLAIM (commit `810e28a`): "Both arms go through the one guard now, and the
/// fallback is derived from a uid that has had everything a path can act on
/// removed."
///
/// Generated rather than hand-picked: every `uid`/`file` pair must produce a
/// document path *inside* the profiles directory. A guard that covers the field
/// somebody named is exactly what the last four reviews found, so this asserts
/// the class.
#[test]
fn generated_index_names_never_leave_the_profiles_directory() {
    let (_dir, paths) = home();
    let hostile = [
        "../canary",
        "../../canary",
        "..",
        ".",
        "",
        "/",
        "//etc/passwd",
        "/etc/passwd",
        "profiles/../../canary",
        "..\\..\\canary",
        "a/../../canary",
        "canary\0",
        "\0",
        "..%2f..%2fcanary",
        "....//....//canary",
        "~/.ssh/authorized_keys",
        "../profiles",
        "../profiles.yaml",
        "profiles.yaml",
        "a/b/c",
        "\n..\n",
        " .. ",
        "...",
        "..yaml",
        ".yaml",
        "canary.yaml/..",
        "./..",
        "C:\\canary",
        "\\\\server\\share",
        "  ",
        "\t",
        "🔒.yaml",
        "ｄｏｔ",
        "⁀",
        "....",
        ".. /canary",
        "../ ",
        "/../canary",
    ];
    let kinds = [
        ProfileType::Local,
        ProfileType::Remote,
        ProfileType::Override,
        ProfileType::Script,
        ProfileType::Merge,
    ];
    for uid in hostile {
        for file in hostile {
            for kind in kinds {
                let item = PrfItem {
                    uid: uid.to_owned(),
                    kind,
                    file: Some(file.to_owned()),
                    ..PrfItem::local("x", "x")
                };
                let path = document_path(&paths, &item);
                assert_eq!(
                    path.parent(),
                    Some(paths.profiles_dir().as_path()),
                    "uid {uid:?} file {file:?} kind {kind:?} produced {path:?}, which \
                     is not one component inside the profiles directory"
                );
                let name = item.file_name();
                assert!(
                    !name.is_empty() && name != "." && name != "..",
                    "uid {uid:?} file {file:?} kind {kind:?} produced the name {name:?}"
                );
            }
        }
    }
}

/// The same claim, through the operation that actually writes: an import from
/// an untrusted directory. Generated over hostile `uid`/`file` pairs, with a
/// canary standing in for everything outside `profiles/`.
#[test]
fn generated_import_names_never_touch_anything_outside_profiles() {
    let (_dir, paths) = home_with_index("current: L1\nitems: []\n", &[]);
    let source = TempDir::new().unwrap();
    let canary = paths.home().join("canary.txt");
    std::fs::write(&canary, "untouched\n").unwrap();
    let index_backup = std::fs::read_to_string(paths.profiles_index()).unwrap();

    let shapes: [(&str, &str); 10] = [
        ("../canary", "../canary"),
        ("../../canary", "../../canary.yaml"),
        ("..", ".."),
        ("", ""),
        ("/etc/passwd", "/etc/passwd"),
        ("profiles", "../profiles"),
        ("../profiles.yaml", "../profiles.yaml"),
        ("a/b", "a/b"),
        ("..\\canary", "..\\canary"),
        ("ok", "../../canary"),
    ];
    let mut index = String::from("items:\n");
    for (i, (uid, file)) in shapes.iter().enumerate() {
        let _ = std::fmt::Write::write_fmt(
            &mut index,
            format_args!("  - uid: {uid:?}\n    type: local\n    name: n{i}\n    file: {file:?}\n"),
        );
    }
    std::fs::create_dir_all(source.path().join("profiles")).unwrap();
    std::fs::write(source.path().join("profiles.yaml"), &index).unwrap();

    let mut store = ProfileStore::load(&paths).unwrap();
    let report = store.import_from(source.path()).unwrap();

    assert_eq!(report.imported, shapes.len());
    assert_eq!(
        std::fs::read_to_string(&canary).unwrap(),
        "untouched\n",
        "an imported name escaped the profiles directory"
    );
    assert_eq!(
        std::fs::read_to_string(paths.profiles_index()).unwrap(),
        index_backup,
        "the index itself was overwritten by an import"
    );
    store.save().unwrap();
    for item in store.items() {
        assert!(
            item.file_name().find('/').is_none() && item.file_name().find('\\').is_none(),
            "`{}` produced the file name {:?}",
            item.uid,
            item.file_name()
        );
    }
}

/// `remove` is the other half of the same claim: the third review's traversal
/// reached the index itself through `remove`. Generated over the same hostile
/// names, with the index and a canary watched.
#[test]
fn generated_remove_never_deletes_outside_the_profiles_directory() {
    let (_dir, paths) = home();
    let canary = paths.home().join("canary.txt");
    std::fs::write(&canary, "untouched\n").unwrap();

    for uid in [
        "../canary.txt",
        "../profiles.yaml",
        "..",
        "../",
        "a/../../canary.txt",
        "..\\canary.txt",
        "/etc/hostname",
        "../../canary.txt",
    ] {
        let (item, _) = {
            // A hand-written index is the only way a hostile uid reaches the
            // store: `add` normalises.
            let index = format!(
                "current: {uid:?}\nitems:\n  - uid: {uid:?}\n    type: local\n    name: n\n"
            );
            std::fs::write(paths.profiles_index(), index).unwrap();
            let mut store = ProfileStore::load(&paths).unwrap();
            let item = store.items()[0].clone();
            let removed = store.remove(&item.uid).unwrap();
            (removed, store)
        };
        assert!(
            item.is_some(),
            "{uid:?} should have been removed from the index"
        );
        assert_eq!(
            std::fs::read_to_string(&canary).unwrap(),
            "untouched\n",
            "uid {uid:?} deleted something outside the profiles directory"
        );
    }
    assert!(canary.exists());
    assert!(paths.profiles_index().is_file(), "the index must survive");
}

// ============================================= 3. the core as the oracle

/// CLAIM: "`validate::check` has no false positives: a config that the core
/// would accept must produce zero errors." Settled against the core, one
/// document at a time, for the spellings a subscription actually contains.
#[test]
fn the_core_accepts_every_rule_spelling_the_validator_does() {
    if !core_available() {
        println!("SKIP: {CORE} is not present");
        return;
    }
    let rules = [
        "IP-CIDR,1.2.3.0/24,DIRECT",
        "IP-CIDR6,2001:db8::/32,DIRECT",
        "IP-SUFFIX,1.2.3.0/24,DIRECT",
        "SRC-IP-SUFFIX,1.2.3.0/24,DIRECT",
        "SRC-IP-CIDR,10.0.0.0/8,DIRECT",
        "GEOIP,CN,DIRECT",
        "GEOIP,private,DIRECT",
        "GEOIP,lan,DIRECT",
        "SRC-GEOIP,CN,DIRECT",
        "IP-ASN,13335,DIRECT",
        "SRC-IP-ASN,13335,DIRECT",
        "IN-TYPE,HTTP,DIRECT",
        "IN-TYPE,HTTP/SOCKS5,DIRECT",
        "NETWORK,udp,DIRECT",
        "NETWORK,tcp,DIRECT",
        "DSCP,4,DIRECT",
        "UID,1000,DIRECT",
        "PROCESS-NAME,curl,DIRECT",
        "PROCESS-PATH,/usr/bin/curl,DIRECT",
        "PROCESS-NAME-REGEX,.*curl,DIRECT",
        "PROCESS-PATH-REGEX,.*curl,DIRECT",
        "PROCESS-NAME-WILDCARD,*curl*,DIRECT",
        "PROCESS-PATH-WILDCARD,/usr/*,DIRECT",
        "DOMAIN-WILDCARD,*.google.com,DIRECT",
        "DOMAIN-REGEX,.*\\.google\\.com,DIRECT",
        "REMATCH-NAME,re,PROXY",
        "DST-PORT,443,DIRECT",
        "SRC-PORT,1000,DIRECT",
        "IN-PORT,7890,DIRECT",
        "IN-USER,alice,DIRECT",
        "IN-NAME,in,DIRECT",
        "AND,((NETWORK,udp),(DOMAIN-SUFFIX,x.com)),DIRECT",
        "OR,((NETWORK,tcp),(NETWORK,udp)),DIRECT",
        "NOT,((NETWORK,udp)),DIRECT",
        "IP-CIDR,10.0.0.0/8,DIRECT,no-resolve",
        "DOMAIN-SUFFIX,x.com,DIRECT,no-resolve",
        "MATCH,GLOBAL",
        "DOMAIN-SUFFIX,x.com,PASS-RULE",
        "DOMAIN-SUFFIX,x.com,COMPATIBLE",
        "MATCH,DIRECT",
    ];
    for rule in rules {
        let text = format!(
            "mixed-port: 17890\nmode: rule\nproxies:\n  - {{name: PROXY, type: socks5, server: 127.0.0.1, port: 1080}}\nproxy-groups:\n  - {{name: grp, type: select, proxies: [PROXY, DIRECT]}}\nrules:\n  - {rule}\n"
        );
        if !core_accepts(&text) {
            println!("the core itself refuses `{rule}` — not a false positive");
            continue;
        }
        let config = Config::from_yaml(&text).unwrap();
        let errors = errors(&config);
        assert!(
            errors.is_empty(),
            "`{rule}` is accepted by the core and rejected by validate: {errors:?}"
        );
    }
}

/// The same claim for the group and provider shapes. A `proxy-provider`
/// supplies node names the document does not contain, and a rule may legally
/// point at one of them — the validator cannot know them, so it must not call
/// them dangling.
#[test]
fn the_core_accepts_every_group_and_provider_shape_the_validator_does() {
    if !core_available() {
        println!("SKIP: {CORE} is not present");
        return;
    }
    let cases: [(&str, &str); 6] = [
        (
            "a provider's node named directly by a rule",
            "proxy-providers:\n  prov:\n    type: http\n    url: http://127.0.0.1:1/p.yaml\n    path: ./prov.yaml\nproxies: []\nproxy-groups:\n  - {name: grp, type: select, use: [prov]}\nrules:\n  - DOMAIN-SUFFIX,x.com,prov-node\n  - MATCH,grp\n",
        ),
        (
            "a provider used by a group",
            "proxy-providers:\n  prov:\n    type: http\n    url: http://127.0.0.1:1/p.yaml\n    path: ./prov.yaml\nproxies: []\nproxy-groups:\n  - {name: grp, type: url-test, use: [prov]}\nrules:\n  - MATCH,grp\n",
        ),
        (
            "a filter that matches nothing",
            "proxies:\n  - {name: PROXY, type: socks5, server: 127.0.0.1, port: 1080}\nproxy-groups:\n  - {name: grp, type: url-test, proxies: [PROXY], filter: \"^ZZZ\"}\nrules:\n  - MATCH,grp\n",
        ),
        (
            "two catch-alls",
            "proxies: []\nproxy-groups: []\nrules:\n  - MATCH,DIRECT\n  - MATCH,REJECT\n",
        ),
        (
            "a group with no members",
            "proxies: []\nproxy-groups:\n  - {name: grp, type: select}\nrules:\n  - MATCH,grp\n",
        ),
        (
            "a rule-set provider",
            "rule-providers:\n  rs:\n    type: http\n    url: http://127.0.0.1:1/rs.yaml\n    path: ./rs.yaml\n    behavior: domain\nproxies: []\nproxy-groups:\n  - {name: grp, type: select, proxies: [DIRECT]}\nrules:\n  - RULE-SET,rs,DIRECT\n  - MATCH,grp\n",
        ),
    ];
    for (what, body) in cases {
        let text = format!("mixed-port: 17890\nmode: rule\n{body}");
        if !core_accepts(&text) {
            println!("the core itself refuses {what} — not a false positive");
            continue;
        }
        let config = Config::from_yaml(&text).unwrap();
        let errors = errors(&config);
        assert!(
            errors.is_empty(),
            "{what} is accepted by the core and rejected by validate: {errors:?}"
        );
    }
}

/// CLAIM: a config the core will reject must be an *error*, so that
/// `Service::start_core` and `cvt apply` refuse it before anything is written.
///
/// A proxy group with no members at all is refused by the core, for every one
/// of the four types:
///
/// ```text
/// level=error msg="proxy group[0]: g: `use` or `proxies` missing"
/// ```
///
/// The check that was meant to catch it is gated on `GroupKind::is_testable()`
/// — a property that answers *does this group have a test URL*, which excludes
/// `select` precisely because a select group has none. So a `select` group
/// produces no diagnostic at all, and the other three produce a warning, not
/// the error the core's refusal calls for.
#[test]
fn defect_6_a_group_with_no_members_is_refused_by_the_core_and_accepted_by_the_validator() {
    if !core_available() {
        println!("SKIP: {CORE} is not present");
        return;
    }
    let shapes = [
        ("select, no members", "- {name: g, type: select}"),
        (
            "select, empty list",
            "- {name: g, type: select, proxies: []}",
        ),
        (
            "url-test, no members",
            "- {name: g, type: url-test, url: http://x/generate_204, interval: 300}",
        ),
        (
            "fallback, no members",
            "- {name: g, type: fallback, url: http://x/generate_204, interval: 300}",
        ),
        (
            "load-balance, no members",
            "- {name: g, type: load-balance}",
        ),
    ];
    for (what, group) in shapes {
        let text = format!(
            "mixed-port: 17890\nmode: rule\nproxies:\n  - {{name: P, type: socks5, server: 127.0.0.1, port: 1080}}\nproxy-groups:\n  {group}\nrules:\n  - MATCH,DIRECT\n"
        );
        assert!(
            !core_accepts(&text),
            "the core now accepts `{what}`; the premise of this test is stale"
        );
        let config = Config::from_yaml(&text).unwrap();
        let report = validate::check(&config);
        let found: Vec<&str> = report.diagnostics.iter().map(|d| d.code).collect();
        assert!(
            report.errors_iter().count() > 0,
            "the core refuses `{what}` ({group}) with `use` or `proxies` missing \
             and the validator reports only {found:?}"
        );
    }
}

/// The core as an oracle has to run on a document a real subscription service
/// wrote, not only on shapes this file invented. The subscription in the
/// workspace is stale — one of the nodes each of its groups references is not
/// in its `proxies` block — and the core refuses it by name. The validator must
/// not be *quieter* than the core about that.
#[test]
fn the_workspace_subscription_is_flagged_at_least_as_loudly_as_the_core() {
    let path = StdPath::new("/home/octo/Desktop/proxy-workflow/sub-octokou.yaml");
    if !path.is_file() || !core_available() {
        println!("SKIP: the workspace subscription or {CORE} is not present");
        return;
    }
    let text = std::fs::read_to_string(path).unwrap();
    let config = Config::from_yaml(&text).unwrap();
    let report = validate::check(&config);
    let found: Vec<&str> = report.diagnostics.iter().map(|d| d.code).collect();
    println!("the validator reports {found:?}");
    if core_accepts(&text) {
        assert!(
            report.errors_iter().count() == 0,
            "the core accepts the subscription and the validator rejects it: {found:?}"
        );
    } else {
        assert!(
            report.errors_iter().count() > 0,
            "the core refuses the subscription and the validator has no error \
             for it: {found:?}"
        );
    }
}

/// CLAIM (commit `810e28a`, the fix for the fourth review): the CIDR family
/// check "stays with the three kinds whose name states one", with `E-CIDR-FAMILY`
/// as an **error** for a payload of the other family.
///
/// The core does not agree, and the rule it accepts is not merely parsed — it
/// is loaded and *active*. Started with
///
/// ```text
/// rules:
///   - IP-CIDR,2001:db8::/32,DIRECT
///   - IP-CIDR6,10.0.0.0/8,REJECT
///   - SRC-IP-CIDR,2001:db8::/32,DIRECT
///   - MATCH,DIRECT
/// ```
///
/// a real `v1.19.31` core answers `GET /rules` with all three, `type` and
/// `payload` intact:
///
/// ```text
/// IPCIDR    2001:db8::/32   DIRECT
/// IPCIDR    10.0.0.0/8      REJECT
/// SrcIPCIDR 2001:db8::/32   DIRECT
/// ```
///
/// The name of the rule type does not constrain the family: `IP-CIDR` and
/// `IP-CIDR6` both build an `IPCIDR` rule, which matches whichever family the
/// payload is. A subscription written that way is refused by `validate`, which
/// means `Service::start_core` will not start a core over it and `cvt apply`
/// will not write it — the "refuses something the core accepts" mistake this
/// project has now made five times, this time introduced *by* the fix.
#[test]
fn defect_7_the_cidr_family_check_refuses_rules_the_core_runs() {
    if !core_available() {
        println!("SKIP: {CORE} is not present");
        return;
    }
    let rules = [
        "IP-CIDR,2001:db8::/32,DIRECT",
        "IP-CIDR6,10.0.0.0/8,REJECT",
        "SRC-IP-CIDR,2001:db8::/32,DIRECT",
        "IP-CIDR6,10.1.2.3/8,DIRECT",
    ];
    for rule in rules {
        let text = format!(
            "mixed-port: 17890\nmode: rule\nproxies: []\nproxy-groups: []\nrules:\n  - {rule}\n"
        );
        assert!(
            core_accepts(&text),
            "the core has started refusing `{rule}`; this test's premise is stale"
        );
        let config = Config::from_yaml(&text).unwrap();
        let found = errors(&config);
        assert!(
            found.is_empty(),
            "the core loads and runs `{rule}` (verified live through `GET /rules`) \
             and validate rejects it with {found:?}"
        );
    }
}

/// The fourth review's exploit class — a *loaded* index entry whose name is
/// unusable — was closed by replacing the bad name rather than refusing it. The
/// replacement deletes every character a path can act on, which is not
/// injective: two distinct uids can map to one document.
///
/// `a/b` cannot be one path component and `a.b` can, so the first falls back to
/// the sanitised stem `ab.yaml` — and the second keeps its own name, which is
/// also `ab.yaml`. Two index entries then share one document: writing one
/// silently replaces the other's subscription, and deleting one removes the
/// document the other still points at. `ProfileStore::add`'s own comment says
/// the funnel exists so that "the index would name one file and the next load
/// would read another"; a *loaded* index never goes through that funnel.
#[test]
fn defect_8_two_index_entries_can_share_one_document() {
    let (_dir, paths) = home_with_index(
        "current: L1\nitems:\n  - {uid: \"a/b\", type: local, name: one}\n  - {uid: \"a.b\", type: local, name: two}\n",
        &[],
    );
    let mut store = ProfileStore::load(&paths).unwrap();
    let one = store.items()[0].clone();
    let two = store.items()[1].clone();

    assert_ne!(
        document_path(&paths, &one),
        document_path(&paths, &two),
        "`{}` and `{}` both name the document {:?}",
        one.uid,
        two.uid,
        one.file_name()
    );

    // And what sharing one costs: the second write destroys the first document.
    store.write_document(&one, "one's subscription\n").unwrap();
    store.write_document(&two, "two's subscription\n").unwrap();
    assert_eq!(
        store.read_document(&one).unwrap(),
        "one's subscription\n",
        "the first profile's document was replaced by the second's"
    );

    store.remove(&one.uid).unwrap();
    assert!(
        store.read_document(&two).is_ok(),
        "removing one profile removed the other's document"
    );
}

// ============================ 4. the newest code of all: 0.5.0's hot reload,
//                                the replay budget, and the backup feature

/// CLAIM (`service.rs`, module docs): "hand the document to the core over the
/// API, which applies most changes without dropping connections; only if that
/// fails, restart the process."
///
/// `Service::hot_reload` sends `PUT /configs {"path": "<home>/runtime/config.yaml"}`.
/// The core is started with `-d <home>/core/work`, and mihomo refuses any
/// `path` that is not under that directory or `SAFE_PATHS`:
///
/// ```text
/// 400 {"message":"path is not subpath of home directory or SAFE_PATHS:
///      …/runtime/config.yaml \n allowed paths: […/core/work]"}
/// ```
///
/// So the API path can never succeed. `ReloadMode::HotReload` — documented as
/// "only ever use the API; a failure is reported rather than worked around" —
/// always reports a failure, and `ReloadMode::Auto`, the default, always falls
/// back to a **restart**, which is the one thing the design exists to avoid.
/// Observed end to end with the real CLI before this test was written:
///
/// ```text
/// $ clash-verge-tui --home … config generate --apply      # core running, pid 30
/// reload         restarted (pid 72)
/// $ clash-verge-tui --home … config generate --apply --mode hot
/// error: could not apply the configuration: core api PUT /configs?force=true
///        returned 400: path is not subpath of home directory or SAFE_PATHS
/// ```
///
/// The client's own doc comment names the precondition this call breaks:
/// [`Client::reload_configs`] takes "the file at `path` (absolute, **under the
/// working directory**)". The contract test uses `/tmp/mh/config.yaml`, and a
/// hand-rolled controller cannot tell an allowed path from a refused one, which
/// is why four reviews of the client did not catch it.
#[tokio::test]
async fn defect_9_the_core_refuses_the_hot_reload_path() {
    if !core_available() {
        println!("SKIP: {CORE} is not present");
        return;
    }
    let (_dir, paths) = home();
    std::fs::write(paths.settings_file(), format!("core:\n  binary: {CORE}\n")).unwrap();

    // The port is probed, released, and then taken by the core, and another
    // real core — a parallel test's, or one an interrupted earlier run leaked —
    // can take it inside that window. That is a race in this harness and not in
    // the code, so it is retried rather than reported; every assertion below is
    // unchanged, and a port that stays unreachable for three attempts still
    // fails the test rather than skipping it.
    let mut service = None;
    for _ in 0..3 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        // The layout `Service` builds for itself, filled in by hand so the core
        // can be started without a profile existing.
        let document = format!(
            "mixed-port: 0\nexternal-controller: 127.0.0.1:{port}\nmode: rule\nproxies:\n  - {{name: node-a, type: socks5, server: 127.0.0.1, port: 1080}}\nproxy-groups:\n  - {{name: grp-select, type: select, proxies: [node-a, DIRECT]}}\nrules:\n  - MATCH,grp-select\n"
        );
        std::fs::write(paths.runtime_config(), &document).unwrap();
        let candidate = Service::open(paths.clone()).unwrap();
        if candidate.start_core().is_ok() {
            service = Some(candidate);
            break;
        }
        let _ = candidate.stop_core();
    }
    let service = service.expect("the core must start for this test to mean anything");
    let _guard = RunningCore(service.clone());

    service
        .wait_until_ready()
        .await
        .expect("the core must become ready before testing hot reload");

    let outcome = service.hot_reload().await;
    let shown = outcome
        .as_ref()
        .err()
        .map(std::string::ToString::to_string)
        .unwrap_or_default();
    assert!(
        outcome.is_ok(),
        "putting the configuration back the way the core already has it is the \
         most reloadable change there is, and the API refused it: {shown}"
    );
}

/// Stops the core whenever the test that started it ends, including on a
/// panic: a leaked core would hold the port for the next run.
struct RunningCore(Service);

impl Drop for RunningCore {
    fn drop(&mut self) {
        let _ = self.0.stop_core();
    }
}

/// The newest fix to the replay, attacked the way the fix itself attacks the
/// version before it: `read_group` puts a deadline on every *read*, but the
/// `select` call between two reads is not inside any budget at all. A core that
/// answers the group and never answers the select therefore costs the client's
/// full request timeout — five seconds, from the settings, not the replay's
/// two — which is the same "a deadline this function cannot enforce is a
/// deadline in name only" the fix was written about, one call further down.
#[tokio::test]
async fn defect_10_the_select_call_is_not_inside_any_budget() {
    let panel = core_with(&[("grp-select", "select")]);
    panel.state.lock().unwrap().hangs_on_select = true;
    let (_dir, service) = service_with_selected(
        &panel.endpoint(),
        "      - name: grp-select\n        now: node-b\n",
    );

    let started = Instant::now();
    let applied = service.restore_selections().await.unwrap();
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_millis(2500),
        "the replay took {elapsed:?} ({applied} restored) against a core that \
         answers reads and not writes; the replay's whole budget is 2 s and the \
         unbounded call costs the client's ten-second timeout"
    );
}

/// The fix for the starvation above gave each group 300 ms of its own, inside a
/// 2 s total. The window the replay exists to wait through is the one a reload
/// leaves behind, and the whole point of `wait_for_group` is that the core
/// takes *some* time to rebuild its groups. A window longer than one group's
/// 300 ms now loses that group's choice outright — silently, because zero is
/// also the ordinary answer for a profile nobody has chosen a node in.
///
/// Twenty 404s at the 25 ms step is a 500 ms window, which the 2 s total budget
/// covers comfortably; the per-group budget does not.
#[tokio::test]
async fn defect_11_a_reload_window_longer_than_one_groups_budget_loses_the_choice() {
    let panel = core_with(&[("grp-select", "select")]);
    panel
        .state
        .lock()
        .unwrap()
        .absent
        .insert("grp-select".to_owned(), 20);
    let (_dir, service) = service_with_selected(
        &panel.endpoint(),
        "      - name: grp-select\n        now: node-b\n",
    );

    let applied = service.restore_selections().await.unwrap();
    assert_eq!(
        applied, 1,
        "the group came back 500 ms into a 2 s budget and the choice was \
         dropped anyway: {applied} restored"
    );
}

/// `Service::restore` guards the *shape* of the directory it reads — it must
/// hold a `profiles.yaml` — and says nothing about whether that directory is
/// also the one it is about to write. The home always holds a `profiles.yaml`,
/// so the most likely wrong directory there is passes the check.
///
/// `std::fs::copy(x, x)` truncates: the file is opened for writing before a
/// byte is read, and the copy reports success having written nothing. A restore
/// whose source is its own destination therefore empties the index, the
/// settings, and every document in `profiles/` and `overrides/` — silently,
/// exit zero. (The safety backup this takes first does hold the data, which is
/// what makes it a data-loss bug rather than an unrecoverable one.)
///
/// The CLI cannot reach this today: it looks a backup up by name inside
/// `backups/`. It is the method's own contract that is wrong, and the contract
/// is public API.
#[test]
fn defect_12_restoring_a_home_onto_itself_empties_the_index_and_every_document() {
    let (_dir, paths) = home_with_index(
        &index_with("      - name: grp-select\n        now: node-b\n"),
        &[
            ("L1.yaml", "mode: rule\nrules:\n  - MATCH,DIRECT\n"),
            ("other.yaml", "mixed-port: 7890\n"),
        ],
    );
    std::fs::write(paths.settings_file(), "ui:\n  refresh_ms: 250\n").unwrap();
    let service = Service::open(paths.clone()).unwrap();

    let home = paths.home().to_path_buf();
    // Either answer removes the data loss this test is about. The fix refuses a
    // restore whose source *is* the destination — clearer than succeeding at
    // nothing — and `copy_file` additionally refuses to copy a file onto
    // itself, so the no-op answer is safe too. Both branches are asserted.
    match service.restore(&home) {
        Ok(safety) => assert!(
            safety.join("profiles.yaml").is_file(),
            "the state it replaced must be kept: {}",
            safety.display()
        ),
        Err(error) => assert!(
            error.to_string().contains("is this home"),
            "a refusal has to say what was wrong: {error}"
        ),
    }
    assert_eq!(
        service.store().unwrap().items().len(),
        1,
        "the index was emptied by a restore that read the home it was writing"
    );
    assert!(
        std::fs::read_to_string(paths.profiles_dir().join("L1.yaml"))
            .is_ok_and(|body| !body.is_empty()),
        "the profile document was emptied"
    );
    assert!(
        std::fs::read_to_string(paths.settings_file()).is_ok_and(|body| !body.is_empty()),
        "the settings file was emptied"
    );
}

// ==================================== 5. the log rotation, as it stands now

/// CLAIM (commit `810e28a`): "prune_logs deleted names this module never writes
/// (`core.log.0`, `.01`, `.+1`)". The complement matters just as much: it must
/// delete the names it *does* write, and nothing else in the directory.
#[test]
fn prune_deletes_only_the_names_rotation_writes() {
    use std::time::SystemTime;

    let (_dir, paths) = home();
    let supervisor = Supervisor::new(paths.clone());
    let log = paths.core_log();
    let dir = paths.logs_dir();
    let old = |name: &str| {
        let path = dir.join(name);
        std::fs::write(&path, "x").unwrap();
        let file = std::fs::File::open(&path).unwrap();
        file.set_modified(SystemTime::now() - Duration::from_secs(40 * 86_400))
            .unwrap();
        path
    };
    let kept = [
        "core.log.1",                          // written by rotate: must go when old
        "core.log.2",                          // ditto
        "core.log.0",                          // never written here
        "core.log.01",                         // ditto
        "core.log.+1",                         // ditto
        "core.log.007",                        // ditto
        "core.log",                            // the live log
        "core.log.backup",                     // someone else's
        "core.log.1.old",                      // ditto
        "app.log.1",                           // the other log's rotation
        "core.logger.1",                       // the stem, not a prefix to be sloppy about
        "core.log.1.tmp",                      // the atomic writer's temporary
        "core.log.99999999999999999999999999", // not a usize
    ];
    for name in kept {
        old(name);
    }
    std::fs::write(&log, "live\n").unwrap();

    let removed = supervisor.prune_logs(&log, 14).unwrap();

    assert_eq!(
        removed, 2,
        "only core.log.1 and core.log.2 are this module's"
    );
    for name in kept {
        let exists = dir.join(name).exists();
        let should_live = !matches!(name, "core.log.1" | "core.log.2");
        assert_eq!(
            exists, should_live,
            "{name}: exists={exists}, expected {should_live}"
        );
    }
}

/// CLAIM (commit `810e28a`): "a symlinked log was rotated by renaming the link,
/// which left the bytes it pointed at in place". The fix copies and truncates.
/// The class question is whether a *rotated* name that is itself a symlink can
/// be made to write through the link.
#[test]
fn rotation_never_writes_through_a_symlink() {
    let (_dir, paths) = home();
    let supervisor = Supervisor::new(paths.clone());
    let log = paths.core_log();
    let dir = paths.logs_dir();
    let canary = paths.home().join("canary.txt");
    std::fs::write(&canary, "untouched\n").unwrap();

    // The log itself is a symlink to a file elsewhere, as a user's redirection
    // would be, and the first rotated name is a symlink to the canary.
    let target = paths.home().join("real.log");
    std::fs::write(&target, "x".repeat(4096)).unwrap();
    std::os::unix::fs::symlink(&target, &log).unwrap();
    std::os::unix::fs::symlink(&canary, dir.join("core.log.1")).unwrap();

    let rotated = supervisor.rotate_log(&log, 1024, 4).unwrap();

    assert!(rotated.is_some(), "the log was over the size limit");
    assert_eq!(
        std::fs::read_to_string(&canary).unwrap(),
        "untouched\n",
        "rotation wrote through a symlink named `core.log.1`"
    );
    assert!(
        std::fs::symlink_metadata(&log).unwrap().is_symlink(),
        "the user's redirection must survive rotation"
    );
    assert_eq!(
        std::fs::metadata(&log).unwrap().len(),
        0,
        "and be truncated"
    );
}

/// CLAIM (commit `810e28a`): "`logs.keep` was capped only on the way to disk…
/// a `cvt.yaml` somebody edited, or one written by an older version, is loaded
/// without it." The cap is `KEEP_LIMIT`; a file written by hand must not cost
/// more than that in probes.
#[test]
fn a_hand_edited_keep_is_capped_before_it_costs_anything() {
    let (_dir, paths) = home();
    std::fs::write(
        paths.settings_file(),
        "logs:\n  max_size_bytes: 1\n  keep: 100000000\n  keep_days: 0\n",
    )
    .unwrap();
    let supervisor = Supervisor::new(paths.clone());
    let log = paths.core_log();
    // Over the (deliberately tiny) size limit.
    std::fs::write(&log, "x".repeat(64)).unwrap();
    std::fs::write(paths.logs_dir().join("core.log.63"), "old").unwrap();

    let started = Instant::now();
    let rotated = supervisor.rotate_log(&log, 1, 100_000_000).unwrap();
    let elapsed = started.elapsed();

    assert!(rotated.is_some());
    assert!(
        elapsed < Duration::from_secs(2),
        "{elapsed:?} spent honouring a `keep` of a hundred million"
    );
}

/// A last look at the newest module for the pattern the commit names: a guard
/// that covers the field somebody named. This is the same failure `defect_5`
/// records, kept as a one-line statement of the shape a fix has to accept.
#[test]
fn a_selected_entry_is_one_of_a_pair_of_optional_strings() {
    // The reference project's `PrfSelected` deserialises from these three
    // shapes; the second and third are what a front end that knows only one of
    // the two fields writes.
    for shard in [
        "      - name: grp-select\n        now: node-b\n",
        "      - name: grp-select\n        now: null\n",
        "      - name: null\n        now: node-b\n",
    ] {
        let (_dir, paths) = home_with_index(
            &index_with(shard),
            &[("L1.yaml", "mode: rule\nrules:\n  - MATCH,DIRECT\n")],
        );
        let loaded = ProfileStore::load(&paths);
        assert!(
            loaded.is_ok(),
            "the reference format accepts the entry `{shard}` and this project \
             does not"
        );
    }
}
