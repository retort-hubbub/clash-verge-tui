//! A **fourth**, independent pass over `cvt-core`, aimed at the newest work.
//!
//! Written against the author's claims, not their tests. The rules this file
//! follows:
//!
//! * the newest code is attacked first — log rotation and the subscription
//!   metadata parsing — because nobody has reviewed it;
//! * a claim that is behavioural over a space of inputs is attacked with a
//!   generated case rather than the input the author happened to pick;
//! * every claim about what mihomo accepts is settled by running the real core
//!   (`/usr/bin/verge-mihomo`, `v1.19.31`) with `-t`, which exits on its own;
//! * tests named `defect_*` assert the behaviour the *claim* promises and are
//!   expected to fail. A failing `defect_` test is a finding, not a broken test.
//!
//! Nothing here starts a long-running core, and nothing here modifies a source
//! file.

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
use std::time::{Duration, Instant, SystemTime};

use cvt_core::AppPaths;
use cvt_core::enhance::overlay::Overlay;
use cvt_core::mihomo::supervisor::{CoreStatus, Supervisor};
use cvt_core::model::config::Config;
use cvt_core::profile::item::{PrfItem, ProfileType};
use cvt_core::profile::source::SubscriptionFetcher;
use cvt_core::profile::store::{ProfileStore, document_path};
use cvt_core::{Service, Settings, validate};
use serde_json::{Value, json};
use tempfile::TempDir;

// ------------------------------------------------------------------ helpers

/// The core this project validates against; `-t` exits on its own.
const CORE: &str = "/usr/bin/verge-mihomo";

fn home() -> (TempDir, AppPaths) {
    let dir = TempDir::new().unwrap();
    let paths = AppPaths::new(dir.path());
    paths.ensure_dirs().unwrap();
    (dir, paths)
}

fn config(text: &str) -> Config {
    Config::from_yaml(text).unwrap_or_else(|e| panic!("the config must parse: {e}\n{text}"))
}

/// The codes `validate` reports, errors and warnings alike.
fn codes(c: &Config) -> Vec<&'static str> {
    validate::check(c)
        .diagnostics
        .iter()
        .map(|d| d.code)
        .collect()
}

fn errors(c: &Config) -> Vec<&'static str> {
    validate::check(c).errors_iter().map(|d| d.code).collect()
}

/// A minimal document holding one rule.
fn doc_with(rule: &str) -> String {
    format!("mixed-port: 7890\nmode: rule\nproxies: []\nproxy-groups: []\nrules:\n  - {rule}\n")
}

/// Ask the real core whether it will load a document. The verdict is
/// `test is successful` on stdout; the log lines go there too.
fn core_accepts(text: &str) -> bool {
    assert!(
        StdPath::new(CORE).is_file(),
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

/// Backdate a file (or a directory) so a day-based cutoff sees it.
fn backdate(path: &StdPath, days: u64) {
    let file = std::fs::File::open(path).unwrap();
    let when = SystemTime::now() - Duration::from_secs(days * 86_400 + 60);
    file.set_modified(when)
        .unwrap_or_else(|e| panic!("could not backdate {}: {e}", path.display()));
}

/// Every name in a directory, sorted.
fn names(dir: &StdPath) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    out.sort();
    out
}

/// Every file under `root` as `relative path -> contents`, with the directory
/// documents are allowed to live in excluded.
fn outside_profiles(root: &StdPath) -> BTreeMap<String, String> {
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
            if rel == "profiles" {
                continue;
            }
            if path.is_dir() {
                out.insert(format!("{rel}/"), String::new());
                walk(&path, root, out);
            } else {
                out.insert(
                    rel,
                    std::fs::read_to_string(&path).unwrap_or_else(|_| "<not utf-8>".to_owned()),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn write(path: &StdPath, bytes: usize) {
    std::fs::write(path, "x".repeat(bytes)).unwrap();
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
    cvt_core::model::rule::Rule::parse(text).is_some_and(|r| r.is_terminal())
}

/// A panel that answers with one canned response per connection, recording the
/// request line it saw. One request per connection, then the socket closes.
struct Panel {
    port: u16,
    seen: Arc<Mutex<Vec<String>>>,
}

impl Panel {
    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    fn targets(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
}

fn response(headers: &[(&str, &str)], body: &str) -> String {
    use std::fmt::Write as _;

    let mut out = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/yaml\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (key, value) in headers {
        let _ = write!(out, "{key}: {value}\r\n");
    }
    out.push_str("\r\n");
    out.push_str(body);
    out
}

const SUBSCRIPTION: &str = "mixed-port: 7890\nmode: rule\nrules:\n  - MATCH,DIRECT\n";

/// Start a panel that serves `responses` in order.
fn panel(responses: Vec<String>) -> Panel {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let listener = tokio::net::TcpListener::from_std(listener).unwrap();
    tokio::spawn(async move {
        let mut index = 0;
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let mut buf: Vec<u8> = Vec::new();
            let mut chunk = [0u8; 1024];
            loop {
                match socket.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&chunk[..n]);
                        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                }
            }
            let head = String::from_utf8_lossy(&buf).into_owned();
            sink.lock()
                .unwrap()
                .push(head.lines().next().unwrap_or_default().to_owned());
            let body = responses.get(index).cloned().unwrap_or_default();
            index += 1;
            let _ = socket.write_all(body.as_bytes()).await;
            let _ = socket.shutdown().await;
        }
    });
    Panel { port, seen }
}

/// A service with settings written the way a user writes them: by hand, in
/// `cvt.yaml`.
fn service_with(settings_yaml: &str) -> (TempDir, Service) {
    let (dir, paths) = home();
    std::fs::write(paths.settings_file(), settings_yaml).unwrap();
    let service = Service::open(paths).unwrap();
    (dir, service)
}

/// A base document, and one that declares no control plane at all.
const BASE: &str = "mixed-port: 7890\nexternal-controller: 127.0.0.1:9090\nmode: rule\n\
                    proxies: []\nproxy-groups: []\nrules:\n  - MATCH,DIRECT\n";
const BASE_NO_PLANE: &str = "mixed-port: 7890\nmode: rule\nproxies: []\nproxy-groups: []\n\
                             rules:\n  - MATCH,DIRECT\n";

/// Seed a service's store with a base and any patches.
fn seed(service: &Service, base: &str, patches: &[(PrfItem, &str)]) {
    let mut store = service.store().unwrap();
    let uid = store.add(PrfItem::local("L1", "base"));
    let item = store.get(&uid).unwrap().clone();
    store.write_document(&item, base).unwrap();
    store.set_current(&uid).unwrap();
    for (item, body) in patches {
        let uid = store.add(item.clone());
        let stored = store.get(&uid).unwrap().clone();
        store.write_document(&stored, body).unwrap();
    }
    store.save().unwrap();
}

// =================================================================== rotation

/// CLAIM: "Rotation happens when the core is *started*, which is the only
/// moment it can be: the child holds the file open while it runs … At a start
/// there is no child, and the file is about to be reopened."
///
/// `Service::start_core` rotates **before** `Supervisor::start` checks whether
/// a core is already running, so a start that is then refused still moves the
/// live log out from under the running child — the exact failure the doc
/// comment above the call says the timing exists to prevent. `logs/core.log`
/// is gone, the running core keeps writing into `logs/core.log.1`, and nothing
/// recreates `core.log`, because the start that would have opened it never
/// happened.
#[test]
fn defect_1_a_start_that_is_refused_still_rotates_the_live_log() {
    let (_dir, service) = service_with(&format!(
        "core:\n  binary: {CORE}\nlogs:\n  max_size_bytes: 1024\n  keep: 2\n  keep_days: 0\n"
    ));
    let paths = service.paths().clone();
    // A core that is running: the pid file names this process, which is alive,
    // and `start_ticks: 0` means "the field was unavailable, so do not
    // second-guess".
    std::fs::write(
        paths.core_dir().join("mihomo.pid"),
        format!("pid: {}\nsince: 1\nstart_ticks: 0\n", std::process::id()),
    )
    .unwrap();
    assert!(
        matches!(
            Supervisor::new(paths.clone()).status(),
            CoreStatus::Running { .. }
        ),
        "the fixture must look like a running core"
    );
    std::fs::write(paths.runtime_config(), BASE).unwrap();
    write(&paths.core_log(), 4096);
    write(&paths.app_log(), 4096);
    let before = std::fs::read_to_string(paths.core_log()).unwrap();

    let err = service.start_core().unwrap_err();
    assert!(
        err.to_string().contains("already running"),
        "the start must be refused for the reason this test is about: {err:?}"
    );
    assert_eq!(
        std::fs::read_to_string(paths.core_log()).ok().as_deref(),
        Some(before.as_str()),
        "a start that never happened moved the log a live core is writing into; \
         logs/ now holds {:?}",
        names(&paths.logs_dir())
    );
    assert!(
        std::fs::read_to_string(paths.app_log()).is_ok(),
        "the application's log was moved by a start that never happened"
    );
}

/// The control for the defect above: with no core running, the same call
/// rotates, and the file the old contents were moved to holds them.
#[test]
fn rotation_moves_the_contents_and_keeps_the_newest_copies() {
    let (_d, paths) = home();
    let sup = Supervisor::new(paths.clone());
    let log = paths.core_log();

    // keep = 1: the previous copy is dropped, and the live file is moved.
    std::fs::write(paths.logs_dir().join("core.log.1"), "old\n").unwrap();
    write(&log, 2048);
    let moved = sup.rotate_log(&log, 1024, 1).unwrap().unwrap();
    assert_eq!(moved, paths.logs_dir().join("core.log.1"));
    assert_eq!(std::fs::read_to_string(&moved).unwrap().len(), 2048);
    assert!(!log.exists());

    // keep = 0 and max_bytes = 0 are the two off switches.
    write(&log, 4096);
    assert!(sup.rotate_log(&log, 1024, 0).unwrap().is_none());
    assert!(sup.rotate_log(&log, 0, 4).unwrap().is_none());
    assert_eq!(std::fs::read_to_string(&log).unwrap().len(), 4096);

    // Below the limit is left alone; at the limit it rotates.
    assert!(sup.rotate_log(&log, 4097, 4).unwrap().is_none());
    assert!(sup.rotate_log(&log, 4096, 4).unwrap().is_some());
}

/// CLAIM: `prune_logs` deletes "`<log>.<n>` files" — "Only the files this
/// module writes: `<log>.<n>`", where `n` comes from `format!(".{n}")` with
/// `n` a `usize` starting at 1.
///
/// The predicate is not that set. It is "the name starts with the log's name,
/// then a dot, then anything `usize::from_str` accepts", which includes a
/// leading `+`, leading zeros and `0` — names this module never writes, and
/// `core.log.0` is a name other rotators do write.
#[test]
fn defect_2_prune_deletes_names_this_module_never_writes() {
    let (_d, paths) = home();
    let sup = Supervisor::new(paths.clone());
    let log = paths.core_log();
    std::fs::write(&log, "live\n").unwrap();

    let rotated = ["core.log.1", "core.log.2", "core.log.3"];
    let foreign = [
        "core.log.0",   // a rotator that counts from zero writes this
        "core.log.01",  // zero-padded, a `logrotate`-style spelling
        "core.log.+1",  // `usize::from_str` accepts a sign
        "core.log.007", // and any width of zero padding
        "core.log.backup",
        "core.log.1.bak",
        "core.log.", // a dot and nothing after it
        "core.logx", // merely starts with the log's name
        "app.log.1", // the other log's rotated copy
        "core.log",  // the live log itself
    ];
    for name in rotated.iter().chain(foreign.iter()) {
        let path = paths.logs_dir().join(name);
        std::fs::write(&path, "old\n").unwrap();
        backdate(&path, 30);
    }
    backdate(&log, 30);

    let removed = sup.prune_logs(&log, 14).unwrap();
    let left = names(&paths.logs_dir());
    let survivors: Vec<&str> = foreign
        .iter()
        .copied()
        .filter(|name| left.iter().any(|n| n == name))
        .collect();
    assert_eq!(
        survivors.len(),
        foreign.len(),
        "`prune_logs` deleted {} name(s) this module never writes (removed {removed} \
         in all); survivors: {survivors:?}",
        foreign.len() - survivors.len()
    );
    assert_eq!(removed, rotated.len(), "three rotated copies are old");
    assert!(
        std::fs::read_to_string(&log).is_ok(),
        "the live log must never be pruned"
    );
}

/// The other half of the same claim: a *fresh* rotated copy is not pruned, and
/// `keep_days: 0` prunes nothing rather than everything.
#[test]
fn prune_keeps_what_is_recent_and_what_zero_days_means_to_keep() {
    let (_d, paths) = home();
    let sup = Supervisor::new(paths.clone());
    let log = paths.core_log();
    std::fs::write(&log, "live\n").unwrap();
    for n in 1..=3 {
        std::fs::write(paths.logs_dir().join(format!("core.log.{n}")), "new\n").unwrap();
    }
    assert_eq!(sup.prune_logs(&log, 14).unwrap(), 0);
    assert_eq!(sup.prune_logs(&log, 0).unwrap(), 0);
    backdate(&paths.logs_dir().join("core.log.1"), 30);
    assert_eq!(sup.prune_logs(&log, 0).unwrap(), 0, "zero days keeps them");
    assert_eq!(sup.prune_logs(&log, 14).unwrap(), 1);
}

/// CLAIM: "`Supervisor::rotate_log` renames `<log>` to `<log>.1` … Returns the
/// file the old contents were moved to."
///
/// The size test follows a symlink (`std::fs::metadata`), the rename does not
/// (`rename(2)` moves the link). So for a log that *is* a symlink — the usual
/// way to put a log somewhere else — the link is rotated and the file it points
/// at is left exactly where it was, holding every byte it held. The returned
/// path is then a link rather than the old contents, and `prune_logs`
/// eventually unlinks it while the real file is never pruned at all.
#[cfg(unix)]
#[test]
fn defect_3_a_symlinked_log_is_not_rotated() {
    let (_d, paths) = home();
    let sup = Supervisor::new(paths.clone());
    let log = paths.core_log();
    let real = paths.home().join("real.log");
    write(&real, 4096);
    std::os::unix::fs::symlink(&real, &log).unwrap();

    let moved = sup.rotate_log(&log, 1024, 4).unwrap().unwrap();
    assert_eq!(moved, paths.logs_dir().join("core.log.1"));
    // The target is truncated once its bytes have been copied: that is what
    // makes the rotation mean anything for a file the writer may still hold
    // open. Leaving the kilobytes in place — which is what this assertion
    // encoded — is the state the defect produced, and its own message says so.
    assert_eq!(
        std::fs::read_to_string(&real).unwrap().len(),
        0,
        "the target kept the bytes that were supposed to move"
    );
    assert!(
        std::fs::symlink_metadata(&moved)
            .unwrap()
            .file_type()
            .is_file(),
        "`{}` is a symlink, not the old contents of the log",
        moved.display()
    );
}

/// CLAIM: `prune_logs` deletes "`<log>.<n>` files older than a cutoff", and is
/// documented to fail only "when a file exists and cannot be removed".
///
/// A *directory* whose name matches the pattern is neither skipped nor removed:
/// `remove_file` fails and the `?` abandons the whole pass. Which of the copies
/// that should have been deleted survive then depends on the order `read_dir`
/// happens to return, so the same home prunes differently on different
/// filesystems.
#[test]
fn defect_4_one_undeletable_name_abandons_the_whole_prune() {
    let (_d, paths) = home();
    let sup = Supervisor::new(paths.clone());
    let log = paths.core_log();
    std::fs::write(&log, "live\n").unwrap();
    let dir = paths.logs_dir().join("core.log.1");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("something"), "x").unwrap();
    backdate(&dir, 30);
    for n in 2..=3 {
        let p = paths.logs_dir().join(format!("core.log.{n}"));
        std::fs::write(&p, "old\n").unwrap();
        backdate(&p, 30);
    }

    let outcome = sup.prune_logs(&log, 14);
    assert!(
        outcome.is_ok(),
        "a name that is not a file this module wrote must not fail the pass: \
         {outcome:?}; logs/ holds {:?}",
        names(&paths.logs_dir())
    );
    assert_eq!(
        outcome.unwrap(),
        2,
        "the two old copies that can be deleted must be deleted: {:?}",
        names(&paths.logs_dir())
    );
}

/// CLAIM: "`logs.keep` above 64 is refused, and the settings screen does not
/// yet offer the three log options; **they are edited in `cvt.yaml`**."
///
/// The refusal lives in `Settings::validate`, which only `save` calls.
/// `Settings::load` — the path that reads the file the changelog tells the user
/// to edit — does not validate, so `keep: 100000000` is accepted and used.
#[test]
fn defect_5_the_keep_cap_is_enforced_only_where_it_is_saved() {
    let (_d, paths) = home();
    let mut settings = Settings::default();
    settings.logs.keep = 65;
    assert!(
        settings.validate().is_err(),
        "the cap exists, so the test below is about where it is applied"
    );
    assert!(
        settings.save(&paths).is_err(),
        "saving refuses it, which is the half that works"
    );

    std::fs::write(paths.settings_file(), "logs:\n  keep: 100000000\n").unwrap();
    let loaded = Settings::load(&paths);
    assert!(
        loaded.is_err(),
        "the documented edit path accepts a value the cap refuses: {:?}",
        loaded.map(|s| s.logs.keep)
    );
}

/// What the missing cap costs, measured rather than asserted: the same log, the
/// same rotation, two values of `keep`.
#[test]
fn a_large_keep_makes_every_start_scan_every_candidate_name() {
    let (_d, paths) = home();
    let sup = Supervisor::new(paths.clone());
    let log = paths.core_log();
    write(&log, 2048);

    let time = |keep: usize| {
        let start = Instant::now();
        assert!(sup.rotate_log(&log, 1024, keep).unwrap().is_some());
        write(&log, 2048);
        start.elapsed()
    };
    let small = time(4);
    let large = time(200_000);
    println!("keep=4 took {small:?}; keep=200000 took {large:?}");
    assert!(
        large < Duration::from_secs(20),
        "200 000 candidates took {large:?}; the count is linear in `keep`"
    );
}

// ======================================================== subscription metadata

/// The decision `build_request` makes, reproduced.
///
/// It has to be reproduced because `build_request` is private, and that is
/// exactly why `defect_6b` below exists: this is a *model* of the decision, and
/// a model goes stale when the decision changes. The end-to-end check is the
/// one that counts.
fn request_target(url: &str) -> String {
    let valid = |u: &str| {
        reqwest::Url::parse(u.trim())
            .ok()
            .filter(|parsed| matches!(parsed.scheme(), "http" | "https"))
    };
    // The shape check the fix added: no query yet, and an `=` after the first
    // `&`. Attempted *before* the validity check, because the broken shape is
    // a valid URL and validating first is what made the repair unreachable.
    let forgot = !url.contains(['?', '#'])
        && url
            .split_once('&')
            .is_some_and(|(_, rest)| rest.contains('='));
    if forgot
        && let Some(repaired) = cvt_core::profile::source::repair_url(url)
        && valid(&repaired).is_some()
    {
        return repaired;
    }
    if valid(url).is_some() {
        return url.to_owned();
    }
    let Some(repaired) = cvt_core::profile::source::repair_url(url) else {
        return url.to_owned();
    };
    if valid(&repaired).is_some() {
        repaired
    } else {
        url.to_owned()
    }
}

/// CLAIM: "A panel that appends `&token=...` to a URL with no `?` produces one
/// path segment and a 404 … The repair is attempted here rather than at the
/// call site so every route into a subscription benefits from it, and only when
/// the original fails to validate — a URL that is already well-formed is never
/// second-guessed."
///
/// `https://…/subscribe&token=abc` **is** well-formed: `&` is a sub-delim, so
/// `Url::parse` accepts it and the `Err` arm never runs. The repair is dead
/// code for the shape it was written for, and the panel's mistake still 404s.
/// A search over the shapes that do fail to validate finds exactly one input
/// the repair can fire on, and it is not that shape.
#[test]
fn defect_6_the_url_repair_is_never_reached_for_the_shape_it_is_for() {
    // The helper itself is right; it is the wiring that never gets there.
    assert_eq!(
        cvt_core::profile::source::repair_url("http://127.0.0.1:1/sub&token=abc").as_deref(),
        Some("http://127.0.0.1:1/sub?token=abc"),
        "the helper this test is about"
    );
    let valid = |u: &str| {
        reqwest::Url::parse(u.trim())
            .ok()
            .filter(|parsed| matches!(parsed.scheme(), "http" | "https"))
    };
    let candidates = [
        "https://x.example/sub&token=abc",
        "https://x.example/sub?a=1&b=2",
        "https://ho&st.example/sub",
        "https://x.example:99999/sub&t=1",
        "https://x.example/sub&t=1#f",
        "//x.example/sub&t=1",
        "https://x.example/a b&t=1",
        "https://x.example/sub&t=%zz",
        "https://[::1/sub&t=1",
        "https://x.example:&t=1",
        "https://user&pass@x.example/sub",
        "https://x.example/#a&b",
        "ftp://x.example/sub&t=1",
        "https://x.example\\sub&t=1",
        "https://x.example/sub&",
    ];
    let reachable: Vec<String> = candidates
        .iter()
        .filter(|c| valid(c).is_none())
        .filter_map(|c| {
            let repaired = cvt_core::profile::source::repair_url(c)?;
            valid(&repaired)
                .is_some()
                .then(|| format!("{c} -> {repaired}"))
        })
        .collect();
    println!("the repair can fire on: {reachable:?}");
    assert_eq!(
        request_target("http://127.0.0.1:1/sub&token=abc"),
        "http://127.0.0.1:1/sub?token=abc",
        "`validated_url` accepts the broken shape, so `build_request` never calls \
         `repair_url`; the repair can fire on {reachable:?}"
    );
    // The `reachable` list answered a different question — which *invalid*
    // URLs the old wiring could rescue — and the answer was "almost none",
    // because the shape it exists for is valid. What matters now is the
    // decision itself, so it is asserted on the shapes that must and must not
    // be touched.
    assert_eq!(
        request_target("https://x.example/sub&token=abc"),
        "https://x.example/sub?token=abc",
        "the shape it was written for is repaired"
    );
    for untouched in [
        // An ampersand in a path, with nothing that looks like a parameter.
        "https://ho&st.example/sub",
        "https://user&pass@x.example/sub",
        // A query string that already has its question mark.
        "https://x.example/sub?a=1&b=2",
        // Nothing after the ampersand to become a query.
        "https://x.example/sub&",
    ] {
        assert_eq!(
            request_target(untouched),
            untouched,
            "must not be rewritten"
        );
    }
}

/// The same claim, end to end: a panel reached over a real socket sees the
/// target the fetcher actually sent.
#[tokio::test]
async fn defect_6b_a_real_fetch_sends_the_question_mark_the_panel_forgot() {
    let (_d, paths) = home();
    let mut store = ProfileStore::load(&paths).unwrap();
    let panel = panel(vec![response(&[], SUBSCRIPTION)]);
    let url = panel.url("/sub&token=abc");
    let uid = store.add(PrfItem::remote("", "panel", url));
    let fetcher = SubscriptionFetcher::new(None).unwrap();
    fetcher.update(&mut store, &uid).await.unwrap();

    let targets = panel.targets();
    assert_eq!(targets.len(), 1, "one tier delivered it: {targets:?}");
    // `targets` holds whole request lines (`GET /path HTTP/1.1`), so this is a
    // containment check: the old `ends_with` could never be true, and the
    // message it printed showed the repair working while the assertion failed.
    assert!(
        targets[0].contains("/sub?token=abc"),
        "the panel's forgotten question mark is still missing on the wire: {:?}",
        targets[0]
    );
}

/// CLAIM: the suggestion is "a name", and both arms of the expression are the
/// same kind of thing — `disposition_filename` is written to produce one path
/// component (`file_stem` is what strips a directory), and `name_from_url` is
/// the other arm.
///
/// `name_from_url` takes the last segment *before* decoding and decodes it
/// afterwards, so a percent-encoded separator and a percent-encoded `..` arrive
/// intact: the suggestion is `../../etc/passwd`. Nothing uses `PrfItem::name`
/// as a path today, which is why this is a low-severity finding rather than a
/// traversal — but it is the one derived string in this feature that is not run
/// through the same guard as the other two (`single_component`, `file_stem`),
/// and the asymmetry is inside a single expression.
#[tokio::test]
async fn defect_7_a_name_from_the_url_keeps_decoded_separators() {
    let (_d, paths) = home();
    let mut store = ProfileStore::load(&paths).unwrap();
    let panel = panel(vec![response(&[], SUBSCRIPTION)]);
    let url = panel.url("/sub/%2e%2e%2f%2e%2e%2fetc%2fpasswd");
    let uid = store.add(PrfItem::remote("", "panel", url));
    let fetcher = SubscriptionFetcher::new(None).unwrap();
    let outcome = fetcher.update(&mut store, &uid).await.unwrap();

    let name = outcome
        .suggested_name
        .as_deref()
        .unwrap_or_default()
        .to_owned();
    println!("the suggested name is {name:?}");
    assert!(
        !name.contains('/') && !name.contains('\\') && !name.contains(".."),
        "the suggested name is not one name: {name:?}"
    );
}

/// The control for the arm next to it: the same traversal sent as a
/// `Content-Disposition` filename is stripped to its last component, because
/// `file_stem` runs *after* the decode.
#[test]
fn disposition_filename_strips_a_decoded_traversal() {
    assert_eq!(
        cvt_core::profile::source::disposition_filename(
            "attachment; filename*=UTF-8''%2e%2e%2f%2e%2e%2fetc%2fpasswd.yaml"
        )
        .as_deref(),
        Some("passwd")
    );
    assert_eq!(
        cvt_core::profile::source::disposition_filename("attachment; filename=\"a/b/../c.yaml\"")
            .as_deref(),
        Some("c")
    );
}

/// CLAIM: `disposition_filename` "Handles the two spellings that exist in the
/// wild: `filename*=UTF-8''<pct>` as RFC 5987 defines it, and the plain quoted
/// `filename="..."`".
///
/// The header is split on `;` *before* the quotes are stripped, so a quoted
/// filename containing a semicolon — legal, and common in panel-generated names
/// — is truncated at it, and the rest of the value becomes a part with no
/// `filename=` prefix that is then dropped.
#[test]
fn defect_8_a_quoted_filename_is_cut_at_its_semicolon() {
    assert_eq!(
        cvt_core::profile::source::disposition_filename("attachment; filename=\"My;Airport.yaml\"")
            .as_deref(),
        Some("My;Airport"),
        "a quoted value ends at the closing quote, not at the first `;`"
    );
}

/// The rest of the header parsing, as controls: what it must keep, what it must
/// refuse, and the fact that neither arm panics or returns a separator. The
/// `%00` case is recorded rather than asserted: a NUL-only filename is adopted
/// as a name of one NUL byte, which no file name may contain and which
/// `single_component` would refuse — it is a display name only, so this is an
/// observation, not a defect.
#[test]
fn the_header_parsers_keep_what_they_must_and_refuse_what_they_must_not() {
    use cvt_core::profile::source::{disposition_filename, is_web_url};

    // RFC 5987 wins over the plain spelling, in either order.
    assert_eq!(
        disposition_filename("attachment; filename=\"a.yaml\"; filename*=UTF-8''b.yaml").as_deref(),
        Some("b")
    );
    assert_eq!(
        disposition_filename("attachment; filename*=UTF-8''%E6%9C%BA%E5%9C%BA.yaml").as_deref(),
        Some("机场")
    );
    // Nothing usable is `None`, never an empty or dotted name.
    for header in [
        "inline",
        "attachment",
        "attachment; filename=\"\"",
        "attachment; filename=\".yaml\"",
        "attachment; filename=\".\"",
        "attachment; filename=\"..\"",
        "attachment; filename=\"   \"",
        "attachment; filename*=UTF-8''",
        "attachment; filename*=UTF-8''.yaml",
    ] {
        assert_eq!(disposition_filename(header), None, "{header}");
    }
    // Malformed escapes and a trailing `%` are left as they stand, and a
    // non-UTF-8 escape becomes a replacement character rather than a panic.
    assert_eq!(
        disposition_filename("attachment; filename=\"%zz.yaml\"").as_deref(),
        Some("%zz")
    );
    assert_eq!(
        disposition_filename("attachment; filename=\"a%2.yaml\"").as_deref(),
        Some("a%2")
    );
    assert_eq!(
        disposition_filename("attachment; filename=\"%FF.yaml\"").as_deref(),
        Some("%FF"),
        "the plain spelling is not percent-decoded"
    );
    assert_eq!(
        disposition_filename("attachment; filename*=UTF-8''%FF.yaml").as_deref(),
        Some("\u{fffd}"),
        "the RFC 5987 spelling is, and a non-UTF-8 escape is lossy, not a panic"
    );
    assert_eq!(
        disposition_filename("attachment; filename*=UTF-8''%00").as_deref(),
        Some("\0"),
        "recorded: a NUL-only name is adopted as a display name"
    );
    // A megabyte of hostile header: bounded work, no panic.
    let huge = format!("attachment; filename=\"{}.yaml\"", "a".repeat(1 << 20));
    assert_eq!(disposition_filename(&huge).unwrap().len(), 1 << 20);

    // Only a web URL is a home page.
    assert!(is_web_url("https://panel.example.com/dashboard"));
    assert!(is_web_url("HTTP://PANEL.EXAMPLE.COM"));
    assert!(is_web_url("  https://panel.example.com  "));
    for bad in [
        "javascript:alert(1)",
        " data:text/html,x",
        "file:///etc/passwd",
        "https://example.com/a b",
        "https://example.com/\nSet-Cookie: x",
        "",
    ] {
        assert!(!is_web_url(bad), "{bad:?}");
    }
}

/// CLAIM: "`profile-web-page-url` is recorded on the profile … A panel that has
/// stopped sending the header does not erase the value: what was true once is
/// worth more than a blank."
#[tokio::test]
async fn a_home_recorded_once_survives_a_panel_that_stops_sending_it() {
    let (_d, paths) = home();
    let mut store = ProfileStore::load(&paths).unwrap();
    let answering = panel(vec![
        response(
            &[("profile-web-page-url", "https://panel.example.com/me")],
            SUBSCRIPTION,
        ),
        response(&[], SUBSCRIPTION),
    ]);
    let uid = store.add(PrfItem::remote("", "panel", answering.url("/sub")));
    let fetcher = SubscriptionFetcher::new(None).unwrap();

    fetcher.update(&mut store, &uid).await.unwrap();
    assert_eq!(
        store.get(&uid).unwrap().home.as_deref(),
        Some("https://panel.example.com/me"),
        "the page the panel advertised is not recorded"
    );
    fetcher.update(&mut store, &uid).await.unwrap();
    assert_eq!(
        store.get(&uid).unwrap().home.as_deref(),
        Some("https://panel.example.com/me"),
        "the recorded page was erased by a panel that stopped sending it"
    );
    // And a `javascript:` one is never recorded in the first place.
    let hostile = panel(vec![response(
        &[("profile-web-page-url", "javascript:alert(1)")],
        SUBSCRIPTION,
    )]);
    let other = store.add(PrfItem::remote("", "panel2", hostile.url("/sub")));
    fetcher.update(&mut store, &other).await.unwrap();
    assert_eq!(store.get(&other).unwrap().home, None);
}

// ================================================================ control plane

/// The control plane has two sources: the settings, and a base profile.
/// Everything else is removed, and losing one that should survive is as much a
/// defect as gaining one that should not.
#[test]
fn the_control_plane_comes_from_the_settings_or_the_base_and_nowhere_else() {
    // The setting wins over the base.
    let (_dir, service) = service_with("core:\n  external_controller: 127.0.0.1:7777\n");
    seed(&service, BASE, &[]);
    assert_eq!(
        service
            .generate()
            .unwrap()
            .config
            .get_str("external-controller")
            .as_deref(),
        Some("127.0.0.1:7777"),
        "the setting must win over the base profile"
    );

    // A base that declares none must not erase the setting.
    let (_dir, service) = service_with("core:\n  external_controller: 127.0.0.1:7777\n");
    seed(&service, BASE_NO_PLANE, &[]);
    assert_eq!(
        service
            .generate()
            .unwrap()
            .config
            .get_str("external-controller")
            .as_deref(),
        Some("127.0.0.1:7777")
    );

    // An empty setting is not a setting, so the base's survives.
    let (_dir, service) = service_with("core:\n  external_controller: ''\n");
    seed(&service, BASE, &[]);
    assert_eq!(
        service
            .generate()
            .unwrap()
            .config
            .get_str("external-controller")
            .as_deref(),
        Some("127.0.0.1:9090")
    );

    // Every route an enhancement has, plus the settings' own `other` map.
    let (_dir, service) = service_with("secret: from-the-other-map\n");
    seed(
        &service,
        BASE_NO_PLANE,
        &[
            (
                PrfItem::patch("m1", "merge", ProfileType::Merge),
                "secret: from-a-merge\nexternal-controller: 10.0.0.1:1\n\
                 external-controller-cors:\n  allow-origins: ['*']\n",
            ),
            (
                PrfItem::patch("o1", "override", ProfileType::Override),
                "set:\n  external-controller-tls: 10.0.0.2:2\n  \
                 external-controller-unix: /tmp/x.sock\n\
                 append:\n  secret: [from-an-override]\n",
            ),
        ],
    );
    let outcome = service.generate().unwrap();
    let doc = outcome.config.as_value();
    for key in [
        "secret",
        "external-controller",
        "external-controller-cors",
        "external-controller-tls",
        "external-controller-unix",
    ] {
        assert!(
            doc.get(key).is_none(),
            "`{key}` reached the generated document; warnings: {:?}",
            outcome.warnings
        );
    }
    assert!(
        outcome.warnings.iter().any(|w| w.contains("secret")),
        "a removal the user has to know about: {:?}",
        outcome.warnings
    );
}

/// The other direction: a key the base declares and an enhancement moves or
/// deletes is put back, with a warning, so an override that does not take
/// effect says so.
#[test]
fn a_key_the_base_declares_is_restored_when_an_enhancement_moves_or_drops_it() {
    let (_dir, service) = service_with("");
    seed(
        &service,
        BASE,
        &[
            (
                PrfItem::patch("m1", "merge", ProfileType::Merge),
                "external-controller: null\n",
            ),
            (
                PrfItem::patch("o1", "override", ProfileType::Override),
                "set:\n  secret: attacker\n",
            ),
        ],
    );
    let outcome = service.generate().unwrap();
    assert_eq!(
        outcome.config.get_str("external-controller").as_deref(),
        Some("127.0.0.1:9090"),
        "a merge deleted the base's endpoint and nothing put it back"
    );
    assert!(
        outcome
            .warnings
            .iter()
            .any(|w| w.contains("external-controller")),
        "{:?}",
        outcome.warnings
    );
}

/// A browser-facing key the list used to exclude, and no longer does.
///
/// `external-ui` decides what the controller serves at `/ui` — the API's own
/// origin — which is the same kind of door as the CORS block the list
/// protects. This was recorded as behaviour rather than reported as a defect,
/// because the exclusion was explicit and reasoned; the reasoning was that the
/// list is about "how this program reaches the core", and the ninth review
/// pointed out that `external-controller-cors` is on it for the other reason
/// and that the other reason covers this key verbatim.
///
/// So the observation is now the opposite, and the assertion says which.
#[test]
fn observation_an_enhancement_cannot_point_the_controller_at_any_directory() {
    let (_dir, service) = service_with("");
    seed(
        &service,
        BASE,
        &[(
            PrfItem::patch("m1", "merge", ProfileType::Merge),
            "external-ui: /home/user\n",
        )],
    );
    let outcome = service.generate().unwrap();
    assert_eq!(
        outcome.config.as_value().get("external-ui"),
        None,
        "an enhancement may not decide what the core serves at `/ui`"
    );
    assert!(
        outcome.warnings.iter().any(|w| w.contains("external-ui")),
        "and it says so: {:?}",
        outcome.warnings
    );
}

/// The import route, which the change names in its own message: a bundle from
/// another installation carries a base profile, and a base profile is allowed
/// to declare the control plane. Recorded as behaviour — it is the documented
/// exception — but it is the one route by which a directory of somebody else's
/// files decides where this program connects.
#[test]
fn an_imported_installation_can_point_the_application_at_its_controller() {
    let (_dir, service) = service_with("");
    let (_foreign_dir, foreign) = home();
    let mut store = ProfileStore::load(&foreign).unwrap();
    let uid = store.add(PrfItem::local("F1", "their base"));
    let item = store.get(&uid).unwrap().clone();
    store
        .write_document(
            &item,
            "mixed-port: 7890\nexternal-controller: 6.6.6.6:6666\nsecret: theirs\nmode: rule\n\
             rules:\n  - MATCH,DIRECT\n",
        )
        .unwrap();
    store.set_current(&uid).unwrap();
    store.save().unwrap();

    let mut mine = service.store().unwrap();
    mine.import_from(foreign.home()).unwrap();
    mine.save().unwrap();
    let outcome = service.generate().unwrap();
    assert_eq!(
        outcome.config.get_str("external-controller").as_deref(),
        Some("6.6.6.6:6666"),
        "the imported base is a base, and a base may declare the control plane"
    );
}

// ============================================== the third round's other fixes

/// CLAIM (third round, `fix(core): a document's name is one path component,
/// like its uid`): "The guard goes in `file_name()` because that is the one
/// place a name becomes a path, so every caller is covered rather than the ones
/// somebody remembered."
///
/// `file_name()` has two arms: `self.file`, which the fix validates, and
/// `default_file_name()`, which is `format!("{uid}.yaml")` — derived from the
/// raw `uid` and **not** validated. When the first arm fails the check the
/// function returns the second, so an index entry that simply omits `file`
/// carries a hostile uid straight through. The author's own example in the
/// `import_from` commit message (`uid: "../profiles"` → `profiles/../profiles.yaml`
/// → the index itself) is still reachable, and `store_fetched` — the call the
/// third review named — is what writes there.
#[test]
fn defect_12_the_uid_derived_fallback_of_file_name_is_not_a_path_component() {
    let (_probe, probe) = home();
    // Every one of these is an index a hand edit, another front end, a restored
    // backup or a synced dotfiles directory can leave behind.
    let shapes: [(String, Option<&str>); 7] = [
        ("../profiles".to_owned(), None),  // the index itself
        ("../../canary".to_owned(), None), // a file above the home
        (format!("{}/canary", probe.home().display()), None), // absolute: `join` discards the base
        ("a/b".to_owned(), None),          // a nested document
        ("..".to_owned(), None),           // a name that is a parent
        ("../x".to_owned(), Some("y.yaml")), // the `file` guard holds: control
        ("L1".to_owned(), Some("../y.yaml")), // and here too: control
    ];

    for (uid, file) in shapes {
        let (_d, paths) = home();
        let file_line = file.map_or_else(String::new, |f| format!(", file: '{f}'"));
        std::fs::write(
            paths.profiles_index(),
            format!(
                "current: null\nitems:\n  - {{uid: '{uid}', type: local, name: x{file_line}}}\n"
            ),
        )
        .unwrap();
        let store = ProfileStore::load(&paths).unwrap();
        let item = store.get(&uid).expect("the entry was loaded").clone();

        let name = item.file_name();
        let one_component =
            !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', '\0']);
        assert!(
            one_component,
            "`file_name()` returned {name:?} for uid {uid:?} file {file:?}; the guard \
             only covers the arm that is not used"
        );

        // And the consequence, if the name were used: a refresh writes the
        // fetched body through `document_path`.
        let before = outside_profiles(paths.home());
        let path = document_path(&paths, &item);
        store.write_document(&item, "FETCHED BODY\n").unwrap();
        let after = outside_profiles(paths.home());
        assert_eq!(
            before,
            after,
            "writing the document for uid {uid:?} changed {} — outside profiles/",
            path.display()
        );
    }
}

/// The same fallback, in the direction the third review's `defect_1` was about:
/// `import_from` computes the **source** path from `item.file_name()` *before*
/// the foreign uid is normalised, so a foreign index entry with a hostile uid
/// and no `file` copies a file from outside the source `profiles/` directory.
#[test]
fn defect_13_import_copies_a_file_from_outside_the_source_profiles_directory() {
    let (_mine_dir, mine_paths) = home();
    let (_foreign_dir, foreign) = home();
    std::fs::write(foreign.home().join("canary.yaml"), "PRECIOUS\n").unwrap();
    std::fs::write(
        foreign.profiles_index(),
        "current: ../canary\nitems:\n  - {uid: ../canary, type: local, name: x}\n",
    )
    .unwrap();

    let mut mine = ProfileStore::load(&mine_paths).unwrap();
    let report = mine.import_from(foreign.home()).unwrap();
    let copied: Vec<String> = std::fs::read_dir(mine_paths.profiles_dir())
        .unwrap()
        .flatten()
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .collect();
    assert!(
        report.documents_copied == 0,
        "the import copied {copied:?} into the profiles directory; a document that \
         lives outside the source `profiles/` directory is not a document"
    );
}

/// The same fallback, through the call the third review named: a **subscription
/// refresh**. `SubscriptionFetcher::update` → `store_fetched` →
/// `store.write_document(&item, &fetched.body)`, and `item` is the loaded index
/// entry. A hand-written index therefore decides where the body of somebody
/// else's subscription is written.
#[tokio::test]
async fn defect_14_a_refresh_writes_the_fetched_body_through_a_hostile_uid() {
    let (_d, paths) = home();
    let canary = paths.home().join("canary.yaml");
    std::fs::write(&canary, "PRECIOUS\n").unwrap();
    let panel = panel(vec![response(&[], SUBSCRIPTION)]);
    let url = panel.url("/sub");
    std::fs::write(
        paths.profiles_index(),
        format!(
            "current: ../canary\nitems:\n  - {{uid: ../canary, type: remote, name: x, url: '{url}'}}\n"
        ),
    )
    .unwrap();

    let mut store = ProfileStore::load(&paths).unwrap();
    let fetcher = SubscriptionFetcher::new(None).unwrap();
    fetcher.update(&mut store, "../canary").await.unwrap();

    assert_eq!(
        std::fs::read_to_string(&canary).unwrap(),
        "PRECIOUS\n",
        "the fetched subscription was written to {}",
        canary.display()
    );
}

/// The class check for the two fixes above, in the shape the fix itself uses:
/// for a loaded index, no field of an entry may reach a path outside
/// `profiles/`, and `document_path` must name one component of it.
#[test]
fn the_file_guard_covers_the_entries_it_was_written_for() {
    let (_d, paths) = home();
    let canary = paths.home().join("canary.yaml");
    std::fs::write(&canary, "PRECIOUS\n").unwrap();
    std::fs::write(
        paths.profiles_index(),
        "current: L1\nitems:\n  \
         - {uid: L1, type: local, name: a, file: '../canary.yaml'}\n  \
         - {uid: L2, type: local, name: b, file: '/tmp/canary.yaml'}\n  \
         - {uid: L3, type: local, name: c, file: 'a/b.yaml'}\n  \
         - {uid: L4, type: local, name: d, file: '..'}\n  \
         - {uid: L5, type: local, name: e, file: '.'}\n  \
         - {uid: L6, type: local, name: f, file: ''}\n",
    )
    .unwrap();
    let store = ProfileStore::load(&paths).unwrap();
    for item in store.items() {
        let path = document_path(&paths, item);
        assert_eq!(
            path.parent().map(StdPath::to_path_buf),
            Some(paths.profiles_dir()),
            "`{}` resolves to {}",
            item.uid,
            path.display()
        );
    }
    let before = std::fs::read_to_string(&canary).unwrap();
    for item in store.items() {
        let _ = store.write_document(item, "mixed-port: 2\n");
        let _ = store.read_document(item);
    }
    assert_eq!(std::fs::read_to_string(&canary).unwrap(), before);
    assert!(canary.is_file());
}

/// CLAIM (third round, `fix(core): IP-CIDR6 exists, and two catch-alls are not
/// an error`): "`IP-CIDR6` was missing from the rule-kind table, so a document
/// containing one was reported as using a type this build does not know."
///
/// The fix is right, and the table is checked against the core below. The class
/// it belongs to is "a payload shape the core refuses, which this build does
/// not mention" — and the arm it was fixed in, `IP-CIDR | IP-CIDR6 |
/// SRC-IP-CIDR`, is the CIDR family, which has five members in the table:
/// `IP-SUFFIX` and `SRC-IP-SUFFIX` take the same prefix-shaped payload and get
/// no check at all. `mihomo -t` refuses a bare address in every one of them.
#[test]
fn defect_9_the_cidr_family_check_covers_three_of_its_five_kinds() {
    // Controls: the kinds the fix named, and the one it added.
    for rule in [
        "IP-CIDR,1.2.3.0/24,DIRECT",
        "IP-CIDR6,2001:db8::/32,DIRECT",
        "SRC-IP-CIDR,1.2.3.0/24,DIRECT",
        "IP-SUFFIX,1.2.3.4/32,DIRECT",
        "SRC-IP-SUFFIX,1.2.3.0/24,DIRECT",
    ] {
        assert!(core_accepts(&doc_with(rule)), "the core loads `{rule}`");
        assert!(
            errors(&config(&doc_with(rule))).is_empty(),
            "the core loads `{rule}`, so this build must not call it an error"
        );
    }
    assert!(
        cvt_core::mihomo::types::is_config_rule_kind("IP-CIDR6"),
        "the fix itself"
    );

    // The other two members of the same family, with the payload the core
    // refuses.
    for rule in ["IP-SUFFIX,1.2.3.4,DIRECT", "SRC-IP-SUFFIX,1.2.3,DIRECT"] {
        assert!(
            !core_accepts(&doc_with(rule)),
            "the premise: `mihomo -t` refuses `{rule}`"
        );
        let report = validate::check(&config(&doc_with(rule)));
        assert!(
            !report.diagnostics.is_empty(),
            "`{rule}` is a document the core refuses and this build says nothing \
             about it; the sibling kind gets `E-CIDR-NO-PREFIX`"
        );
    }
}

/// The same class in the diagnostic next to it: `W-CIDR-NO-PREFIX` said
/// "mihomo assumes a full-length mask", which the core contradicts — it refuses
/// the rule. A document this build calls a warning is a document that cannot
/// start a core.
#[test]
fn defect_10_a_bare_ip_cidr_is_refused_by_the_core_and_only_warned_about_here() {
    let text = "mixed-port: 7890\nexternal-controller: 127.0.0.1:9090\nmode: rule\nproxies: []\n\
                proxy-groups: []\nrules:\n  - IP-CIDR,1.2.3.4,DIRECT\n";
    assert!(
        !core_accepts(text),
        "the premise: `mihomo -t` refuses a bare `IP-CIDR` payload"
    );
    assert!(
        codes(&config(text)).contains(&"E-CIDR-NO-PREFIX"),
        "the diagnostic this test is about: {:?}",
        codes(&config(text))
    );
    assert!(
        !errors(&config(text)).is_empty(),
        "the core refuses this document, so it is not a warning"
    );
}

/// CLAIM (third round, `fix(core): the overlay disagreements the third review
/// found`): "`{a[0].b: 1}` with `append: {a: [...]}`, which is a working
/// overlay, because addressing an element of a list is what an index is *for*."
///
/// The idempotence half holds: across the generated cross-product there is no
/// accepted overlay that applies twice differently. The other half is
/// over-strong — the combination is a working overlay only when the addressed
/// element already exists. Over a document where the list or the element is
/// absent, `validate` accepts it and `apply` then refuses with an error naming
/// the `set`, so the shape the fix was written to allow does not work for the
/// case the fix's comment gives as its example. The count is printed rather
/// than asserted: a document-free `validate` cannot know it.
#[test]
fn the_shape_check_keeps_the_idempotence_it_was_written_for() {
    let docs = [
        json!({}),
        json!({"a": {}}),
        json!({"a": {"b": []}}),
        json!({"a": []}),
        json!({"a": [{"name": "x"}]}),
        json!({"a": [{"b": 0}]}),
        json!({"a": 5}),
        json!({"a": {"b": 5}}),
        json!({"a": null}),
    ];
    let sets: [(&str, Value); 10] = [
        ("a", json!(5)),
        ("a", json!({})),
        ("a", json!([])),
        ("a", json!({"b": [1]})),
        ("a.b", json!(1)),
        ("a.b", json!([1, 2])),
        ("a.b.c", json!(1)),
        ("a[0]", json!(1)),
        ("a[0].b", json!({"c": 1})),
        ("a[0].b", json!(1)),
    ];
    let lists = ["a", "a.b", "a.b.c", "a[0]"];

    let mut accepted = 0;
    let mut cannot_apply: Vec<String> = Vec::new();
    for doc in &docs {
        for (set_path, value) in &sets {
            for list in &lists {
                let overlay = Overlay {
                    set: BTreeMap::from([(set_path.to_string(), value.clone())]),
                    append: BTreeMap::from([(list.to_string(), vec![json!("X")])]),
                    ..Overlay::default()
                };
                if overlay.validate().is_err() {
                    continue;
                }
                accepted += 1;
                let mut once = doc.clone();
                if let Err(e) = overlay.apply(&mut once) {
                    cannot_apply.push(format!(
                        "doc {doc} + set {set_path}={value} + append {list}: {e}"
                    ));
                    continue;
                }
                let mut twice = once.clone();
                overlay.apply(&mut twice).unwrap();
                assert_eq!(
                    once, twice,
                    "set {set_path} + append {list} over {doc} is not idempotent"
                );
            }
        }
    }
    assert!(
        accepted > 100,
        "the generator must actually produce accepted overlays: {accepted}"
    );
    println!(
        "accepted = {accepted}; accepted but not applicable = {}",
        cannot_apply.len()
    );
    for case in cannot_apply.iter().take(3) {
        println!("  {case}");
    }
    assert!(
        !cannot_apply.is_empty(),
        "the claim that this shape is a working overlay: the generator must show \
         the documents where it is not"
    );

    // The same code path in the shape a user actually writes: a list the base
    // does not have. `validate` accepts it (the claim), and `apply` then reports
    // the list as missing.
    let realistic = Overlay {
        set: BTreeMap::from([("dns.nameserver[0]".to_owned(), json!("1.1.1.1"))]),
        append: BTreeMap::from([("dns.nameserver".to_owned(), vec![json!("8.8.8.8")])]),
        ..Overlay::default()
    };
    assert!(realistic.validate().is_ok());
    let mut doc = json!({});
    println!(
        "`dns.nameserver[0]` + `append dns.nameserver` over an empty base: {:?}",
        realistic.apply(&mut doc)
    );
}

/// The name suggestion for the shape every panel actually serves. Recorded
/// rather than reported: the claim says the fallback is the URL's last segment,
/// and this is what that is — a word that names nothing, and a heuristic that
/// drops a legitimate name containing a dot.
#[tokio::test]
async fn observation_the_url_heuristic_suggests_a_word_or_nothing() {
    let (_d, paths) = home();
    let mut store = ProfileStore::load(&paths).unwrap();
    let panel = panel(vec![
        response(&[], SUBSCRIPTION),
        response(&[], SUBSCRIPTION),
    ]);
    let fetcher = SubscriptionFetcher::new(None).unwrap();
    let first = store.add(PrfItem::remote(
        "",
        "a",
        panel.url("/api/v1/client/subscribe?token=abc"),
    ));
    let second = store.add(PrfItem::remote("", "b", panel.url("/sub/My.Airport")));
    let one = fetcher.update(&mut store, &first).await.unwrap();
    let two = fetcher.update(&mut store, &second).await.unwrap();
    println!(
        "suggested names: {:?} and {:?}",
        one.suggested_name, two.suggested_name
    );
    assert_eq!(one.suggested_name.as_deref(), Some("subscribe"));
    // `My.Airport` is now a name rather than nothing: the old arm refused any
    // segment containing a dot, which rejected a perfectly good one to avoid a
    // host name — and it did not need to, because the segment is the *last*
    // one and a host is never last on a path that has a path.
    assert_eq!(two.suggested_name.as_deref(), Some("My.Airport"));
}

/// CLAIM (third round, `fix(core): IP-CIDR6 exists, and two catch-alls are not
/// an error`): "That the append refusal is justified by a document the core
/// rejects was worth checking for exactly this reason, and it turned out to be
/// false: what is refused is a *single patch* naming two catch-alls, where one
/// of them must lose and the log would claim both were added."
///
/// The refusal is on `append` only. The same patch written as a `prepend` is
/// accepted, and both halves of the stated reason then happen: with no
/// catch-all in the base, two catch-alls are prepended and the second can never
/// fire; with one, the first of the two is silently dropped.
#[test]
fn defect_11_two_catch_alls_in_one_prepend_are_not_refused() {
    let patch = |kind: &str| format!("{kind}:\n  rules: [\"MATCH,DIRECT\", \"MATCH,REJECT\"]\n");
    // The control: the append spelling is refused, with the reason.
    assert!(
        Overlay::from_yaml(&patch("append")).is_err(),
        "the append refusal this test is about"
    );

    // Either outcome satisfies what this test is about — the patch is refused,
    // or it leaves exactly one catch-all — and the fix took the first. It was
    // written when the answer was the second.
    let Ok(overlay) = Overlay::from_yaml(&patch("prepend")) else {
        return;
    };
    let mut doc = json!({"rules": ["DOMAIN-SUFFIX,google.com,PROXY"]});
    let log = overlay.apply(&mut doc).unwrap();
    let rules = rules_of(&doc);
    assert_eq!(
        rules.iter().filter(|r| is_terminal(r)).count(),
        1,
        "two catch-alls were prepended and only the first can fire: {rules:?} ({log:?})"
    );

    let mut doc = json!({"rules": ["DOMAIN-SUFFIX,google.com,PROXY", "MATCH,DIRECT"]});
    let log = overlay.apply(&mut doc).unwrap();
    let rules = rules_of(&doc);
    assert!(
        rules.iter().any(|r| r == "MATCH,REJECT"),
        "the patch named two catch-alls and the log claims both were added: \
         {rules:?} {log:?}"
    );
}

/// The claim's control, kept so the tests above cannot pass vacuously: the
/// genuine disagreements are still refused, and the consistent shapes are not.
#[test]
fn the_shape_check_still_refuses_the_disagreements_it_was_written_for() {
    let build = |set_path: &str, value: &Value, list: &str| Overlay {
        set: BTreeMap::from([(set_path.to_string(), value.clone())]),
        append: BTreeMap::from([(list.to_string(), vec![json!("X")])]),
        ..Overlay::default()
    };
    for (set_path, value, list) in [
        ("a", json!("scalar"), "a.b"),
        ("a", json!(5), "a.b"),
        ("a.b", json!("scalar"), "a"),
        ("a.b.c", json!(5), "a"),
    ] {
        assert!(
            build(set_path, &value, list).validate().is_err(),
            "set {set_path} = {value} with append {list} must be refused"
        );
    }
    for (set_path, value, list) in [
        ("a[0]", json!(1), "a"),
        ("a[0].b", json!(1), "a"),
        ("a", json!([]), "a"),
        ("a", json!({}), "a.b"),
    ] {
        assert!(
            build(set_path, &value, list).validate().is_ok(),
            "set {set_path} = {value} with append {list} is a working overlay"
        );
    }
}

/// A last guard on the helper these tests lean on: it must be able to say no.
#[test]
fn the_core_check_can_fail() {
    assert!(core_accepts(&doc_with("MATCH,DIRECT")));
    assert!(!core_accepts(&doc_with("NOPE,MATCH,DIRECT")));
}
