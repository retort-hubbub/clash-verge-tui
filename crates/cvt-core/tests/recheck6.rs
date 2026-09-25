//! A **seventh** independent pass, aimed at the two newest fixes (`0bff23a`,
//! `37e00be`), the backup/restore state machine they touched, and the *class*
//! those two commits kept getting wrong: a guard placed in the command somebody
//! named rather than in the function that owns the invariant.
//!
//! **The tree moved while this file was being written.** The review was told to
//! work at `5c92f28` with the author confined to `crates/cvt/**` and `docs/**`.
//! That held for the first commit inspected and then stopped: `d48a4e8`
//! ("ask where the traffic comes out, and bound the wait for it") landed with
//! `crates/cvt-core/src/service.rs` in it, rewriting `wait_for_document` — one
//! of the two things `37e00be` claimed, and a target of this review. Every
//! finding below therefore names the commit it was taken at, and the ones whose
//! files did *not* move between `5c92f28` and that commit say so:
//!
//! ```text
//! crates/cvt/src/commands/test.rs      unchanged between 5c92f28 and d48a4e8
//! crates/cvt/src/commands/backup.rs    unchanged
//! crates/cvt-core/src/settings.rs      unchanged
//! crates/cvt/src/commands/mod.rs       +2 lines (geo helpers); node_options identical
//! crates/cvt-core/src/service.rs       +88/-16, all of it wait_for_document
//! ```
//!
//! Method, in the order the review was asked to attack:
//!
//! * the newest fixes first, by *running* them: `--concurrency`, `--timeout`,
//!   `wait_for_document`, `copy_dir`, and the backups;
//! * the flag class enumerated mechanically — every `--concurrency` /
//!   `--timeout` / `--url` the CLI accepts, taken one at a time;
//! * the state machine attacked as a whole, not at the cases the last round
//!   named: a hard link between the home and a backup, a name
//!   `free_backup_path` invents, a fifo in `backups/`, a backup of a backup;
//! * the real core at `/usr/bin/verge-mihomo` settles the two claims only it
//!   can: what `mihomo -t` accepts, and what it answers for an out-of-range
//!   `timeout` — which is the stated reason the settings refuse one.
//!
//! Tests named `defect_*` assert what the code claims and are expected to
//! **fail**. A failing `defect_` test is a finding, not a broken test. Tests
//! named `pre_fix_*` assert the shape the code *used to* have, on a
//! reconstruction of it, and are there to show a test would have caught it.
//!
//! # Where each finding stands
//!
//! The author was fixing them as they were reported, so a `defect_*` test that
//! failed an hour ago may pass now. Each doc comment carries the commit it was
//! taken at and the output that was observed; this is the summary, as of
//! `fbf3d85`:
//!
//! ```text
//! defect_1  --concurrency not bounded in `test urls`   FIXED c76fdac (guard)
//! defect_2  --timeout held to the core's int16         FIXED c76fdac (guard)
//! defect_3  copy_file's path guard vs a hard link      FIXED in the working
//!                                                      tree, then c76fdac
//! defect_4  `<stamp>-overflow` parsed back as seq 1    FIXED c76fdac (guard)
//! defect_5  `backup create` reported epoch/0 items     FIXED c76fdac (guard)
//! defect_6  the same number, two verdicts              DESIGN, see 12a83c1
//! defect_7  the clamp is not reported by `test urls`   OPEN
//! ```
//!
//! Nothing here modifies a source file.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    clippy::similar_names,
    clippy::items_after_statements
)]

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cvt_core::mihomo::client::Client;
use cvt_core::mihomo::supervisor::Supervisor;
use cvt_core::settings::{MAX_TEST_CONCURRENCY, MAX_TEST_TIMEOUT_MS, Settings};
use cvt_core::{AppPaths, ReloadMode, Service};
use tempfile::TempDir;

/// The core this project is validated against; `-t` exits on its own.
const CORE: &str = "/usr/bin/verge-mihomo";

fn core_available() -> bool {
    Path::new(CORE).is_file()
}

/// A fresh home with the directories every path helper expects.
fn home() -> (TempDir, AppPaths) {
    let dir = TempDir::new().unwrap();
    let paths = AppPaths::new(dir.path());
    paths.ensure_dirs().unwrap();
    (dir, paths)
}

/// A home whose `profiles.yaml` is written out verbatim, plus documents.
fn home_with_index(index: &str, documents: &[(&str, &str)]) -> (TempDir, AppPaths) {
    let (dir, paths) = home();
    std::fs::write(paths.profiles_index(), index).unwrap();
    for (name, body) in documents {
        std::fs::write(paths.profiles_dir().join(name), body).unwrap();
    }
    (dir, paths)
}

/// An index naming one local profile, with `selected` supplied verbatim.
fn index_with(selected: &str) -> String {
    format!(
        "current: L1\nitems:\n  - uid: L1\n    type: local\n    name: base\n    file: L1.yaml\n    selected:\n{selected}"
    )
}

/// A profile document with a real control plane and one group.
fn base_document(endpoint: &str) -> String {
    format!(
        "mixed-port: 7890\nexternal-controller: {endpoint}\nmode: rule\n\
         proxies:\n  - {{name: node-a, type: socks5, server: 127.0.0.1, port: 1080}}\n\
         proxy-groups:\n  - {{name: PROXY, type: select, proxies: [node-a, DIRECT]}}\n\
         rules:\n  - MATCH,PROXY\n"
    )
}

/// A document declaring two groups, for the `wait_for_document` checks.
fn two_group_document(endpoint: &str) -> String {
    format!(
        "mixed-port: 7890\nexternal-controller: {endpoint}\nmode: rule\n\
         proxies:\n  - {{name: node-a, type: socks5, server: 127.0.0.1, port: 1080}}\n\
         proxy-groups:\n\
         \x20 - {{name: missing-group, type: select, proxies: [node-a, DIRECT]}}\n\
         \x20 - {{name: late-group, type: select, proxies: [node-a, DIRECT]}}\n\
         rules:\n  - MATCH,missing-group\n"
    )
}

// ============================================================ the fake controller

/// The state a fake controller keeps, as the live probes recorded the shapes.
#[derive(Default)]
struct PanelState {
    /// group name -> `select` | `url-test` | `fallback` | `load-balance`
    groups: Vec<(String, String)>,
    members: BTreeMap<String, Vec<String>>,
    now: BTreeMap<String, String>,
    /// The group answers `404` this many more times.
    absent: BTreeMap<String, usize>,
    /// Proxies the controller lists only in its own map, with no group naming
    /// them — the second of "both places a name can be".
    bare: Vec<String>,
    /// Latency the delay endpoint reports, by default and per URL substring.
    delay_ms: u64,
    url_delays: Vec<(String, u64)>,
    /// Requests in flight right now, and the most there have ever been.
    inflight: usize,
    peak_inflight: usize,
    /// When greater than zero, a delay request is held until this many are in
    /// flight at once (or `HOLD_LIMIT` passes). This turns "how many requests
    /// does the client allow at once" into a predicate rather than a race
    /// between a timer and a server's accept loop: a client whose limit is
    /// below the barrier can never trip it, and the peak it reaches is exactly
    /// its limit.
    hold: usize,
    /// Every `/group/<name>` this controller was asked about, in order.
    asked: Vec<String>,
}

impl PanelState {
    /// How long this request should take, for a URL the way the client encodes
    /// it in the query string.
    fn delay_for(&self, target: &str) -> u64 {
        let query = target.split_once('?').map_or("", |(_, q)| q);
        let raw = query
            .split('&')
            .find_map(|pair| pair.strip_prefix("url="))
            .unwrap_or_default()
            .replace("%3A", ":")
            .replace("%2F", "/");
        self.url_delays
            .iter()
            .find(|(needle, _)| raw.contains(needle.as_str()))
            .map_or(self.delay_ms, |(_, ms)| *ms)
    }
}

/// How long a held delay request waits for the barrier before giving up.
const HOLD_LIMIT: Duration = Duration::from_millis(2500);

/// A controller that answers the `/group`, `/proxies` and `/delay` shapes the
/// live probes recorded.
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

    fn peak_inflight(&self) -> usize {
        self.state.lock().unwrap().peak_inflight
    }

    /// Every group this controller was asked about, in order.
    fn asked(&self) -> Vec<String> {
        self.state.lock().unwrap().asked.clone()
    }

    fn delays(&self) -> usize {
        self.requests()
            .iter()
            .filter(|request| request.contains("/delay"))
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
        "load-balance" => {}
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

    if path == "/version" {
        return (200, "{\"version\":\"v1.19.31\",\"meta\":true}".to_owned());
    }
    if path == "/proxies" || path == "/proxies/" {
        let mut entries: Vec<String> = Vec::new();
        for (name, kind) in &state.groups {
            let pascal = match kind.as_str() {
                "url-test" => "URLTest",
                "fallback" => "Fallback",
                "load-balance" => "LoadBalance",
                _ => "Selector",
            };
            let members = state.members.get(name).cloned().unwrap_or_default();
            let all: String = members
                .iter()
                .map(|m| format!("{m:?}"))
                .collect::<Vec<_>>()
                .join(",");
            entries.push(format!(
                "{name:?}:{{\"name\":{name:?},\"type\":{pascal:?},\"all\":[{all}],\"history\":[],\"now\":{:?}}}",
                state.now.get(name).cloned().unwrap_or_default()
            ));
        }
        for name in &state.bare {
            entries.push(format!(
                "{name:?}:{{\"name\":{name:?},\"type\":\"Socks5\",\"all\":[],\"history\":[]}}"
            ));
        }
        return (200, format!("{{\"proxies\":{{{}}}}}", entries.join(",")));
    }
    if path == "/group" || path == "/group/" {
        let groups: Vec<String> = state
            .groups
            .iter()
            .map(|(n, k)| group_json(state, n, k))
            .collect();
        return (200, format!("{{\"proxies\":[{}]}}", groups.join(",")));
    }
    if let Some(name) = path.strip_prefix("/group/") {
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
    // `PUT /configs?force=true` is how a hot reload reaches the core; the real
    // one answers 204 while it rebuilds its groups in the background.
    if (path == "/configs" || path == "/configs/") && method == "PUT" {
        return (204, String::new());
    }
    if let Some(rest) = path.strip_prefix("/proxies/") {
        if let Some(name) = rest.strip_suffix("/delay") {
            let query = target.split_once('?').map_or("", |(_, q)| q);
            let url = query
                .split('&')
                .find_map(|pair| pair.strip_prefix("url="))
                .unwrap_or_default();
            // Verified live: a proxy the core does not know answers 404.
            let known = state.groups.iter().any(|(n, _)| n == name)
                || state.bare.iter().any(|n| n == name)
                || state.members.values().any(|m| m.iter().any(|x| x == name));
            if !known {
                return (404, "{\"message\":\"Resource not found\"}".to_owned());
            }
            if !url.starts_with("http") && !url.starts_with("https%3A") {
                // What the real core answers for a value that is not a URL.
                return (
                    503,
                    "{\"message\":\"An error occurred in the delay test\"}".to_owned(),
                );
            }
            let delay = state.delay_for(target);
            return (200, format!("{{\"delay\":{}}}", delay.max(1)));
        }
        if rest.is_empty() {
            return (405, String::new());
        }
        let name = rest;
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
        if kind == "load-balance" {
            (400, "{\"message\":\"Must be a Selector\"}".to_owned())
        } else {
            state.now.insert(name.to_owned(), member.to_owned());
            (204, String::new())
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
        503 => "Service Unavailable",
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
    let mut state = PanelState {
        delay_ms: 5,
        ..PanelState::default()
    };
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
    serve(state)
}

/// The same, with `count` members on one group, for the concurrency checks.
fn panel_with_nodes(count: usize) -> Panel {
    let mut state = PanelState {
        delay_ms: 1,
        ..PanelState::default()
    };
    state.groups.push(("PROXY".to_owned(), "select".to_owned()));
    state.members.insert(
        "PROXY".to_owned(),
        (0..count).map(|n| format!("node-{n:04}")).collect(),
    );
    state.now.insert("PROXY".to_owned(), "node-0000".to_owned());
    serve(state)
}

/// Serve one `PanelState` on a port of the OS's choosing.
fn serve(state: PanelState) -> Panel {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

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

                // `/group/<name>` is the call `wait_for_document` makes, and the
                // name it asked about is the whole question those tests ask.
                if let Some(rest) = target
                    .split('?')
                    .next()
                    .unwrap_or("")
                    .strip_prefix("/group/")
                {
                    state.lock().unwrap().asked.push(rest.to_owned());
                }

                let is_delay = target.contains("/delay");
                if is_delay {
                    let holding = {
                        let mut guard = state.lock().unwrap();
                        guard.inflight += 1;
                        guard.peak_inflight = guard.peak_inflight.max(guard.inflight);
                        guard.hold > 0
                    };
                    // Polled, never awaited under the lock: every connection
                    // handler has to be able to record itself. The barrier is
                    // disarmed the moment it either trips or gives up, so that
                    // a request arriving after it can never hold for a second
                    // `HOLD_LIMIT` of its own.
                    if holding {
                        let started = std::time::Instant::now();
                        loop {
                            {
                                let mut guard = state.lock().unwrap();
                                let armed = guard.hold;
                                if armed == 0 || guard.inflight >= armed {
                                    guard.hold = 0;
                                    break;
                                }
                                if started.elapsed() >= HOLD_LIMIT {
                                    guard.hold = 0;
                                    // Explicit: the guard is held across the
                                    // `break` otherwise, and the lint is right
                                    // that nothing needs it to be.
                                    drop(guard);
                                    break;
                                }
                            }
                            tokio::time::sleep(Duration::from_millis(5)).await;
                        }
                    } else {
                        let delay = state.lock().unwrap().delay_for(&target);
                        tokio::time::sleep(Duration::from_millis(delay)).await;
                    }
                    let mut guard = state.lock().unwrap();
                    guard.inflight -= 1;
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

// ============================================================ the CLI, by process

/// The CLI this project ships, next to the test binary.
fn cli_binary() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.parent()?;
    ["clash-verge-tui", "cvt"]
        .iter()
        .map(|name| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// A home the CLI can open: an index naming one local profile whose document
/// points the control plane at `endpoint`.
fn cli_home(endpoint: &str) -> (TempDir, AppPaths) {
    home_with_index(&index_with(""), &[("L1.yaml", &base_document(endpoint))])
}

fn run_cli(bin: &Path, home: &Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new(bin)
        .arg("--home")
        .arg(home)
        .args(args)
        .env("NO_COLOR", "1")
        .output()
        .unwrap()
}

fn stdout_of(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A runtime the panel gets to live in while a CLI process runs against it.
fn panel_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap()
}

/// `count` configured test targets, in a settings file of their own.
fn write_targets(paths: &AppPaths, count: usize) {
    let mut text = String::from("test:\n  urls:\n");
    for index in 0..count {
        let _ = writeln!(
            text,
            "    - {{name: u{index:04}, url: https://u{index:04}.example/robots.txt}}"
        );
    }
    std::fs::write(paths.settings_file(), text).unwrap();
}

// =================== 1. the flag class: `--concurrency`, taken one command at a time

/// CLAIM (`37e00be`): "`--concurrency` had a ceiling in one place and not the
/// other. `600` in `cvt.yaml` is refused with \"would exhaust file
/// descriptors\"; `--concurrency 600` went straight to `buffer_unordered`. One
/// limit, two answers, and the one that got through was the one nothing
/// validated." … "So both checks moved into `node_options`, the one function
/// every latency command reads its flags through."
///
/// `node_options` is read by the commands that take `NodeTestArgs` — `proxies
/// test`, `proxies test-all` and `test delay`. `--concurrency` is accepted by a
/// **fourth** command, `cvt test urls`, whose arguments are `UrlsArgs` and
/// which reads its flags itself:
///
/// ```text
/// let concurrency = args.concurrency.unwrap_or(defaults.concurrency).max(1);
/// ```
///
/// `crates/cvt/src/commands/mod.rs` is byte-identical in this respect between
/// `5c92f28` and `d48a4e8` (`+2` lines of `geo` helpers), so the claim is
/// measured against both. The guard covers the commands somebody named, one
/// commit after the commit that says that is the pattern.
///
/// The instrument is a barrier rather than a stopwatch: the controller holds
/// every delay request until `MAX_TEST_CONCURRENCY + 1` are in flight at once,
/// or 2.5 s pass. A client whose limit is 512 can never trip that barrier, so
/// the peak it reaches *is* its limit; the count of delay requests is asserted
/// separately so "the panel never saw the workload" cannot be mistaken for a
/// pass.
#[test]
fn defect_1_the_concurrency_ceiling_guards_three_commands_and_not_the_class() {
    let Some(bin) = cli_binary() else {
        eprintln!("SKIP: no clash-verge-tui binary; run `cargo build -p cvt`");
        return;
    };
    with_barrier(&bin, Barrier::TestUrls);
    with_barrier(&bin, Barrier::TestAll);
}

/// Which command the barrier is pointed at.
#[derive(Clone, Copy)]
enum Barrier {
    /// `test urls --concurrency 600`, over 600 configured URLs.
    TestUrls,
    /// `proxies test-all --concurrency 600`, over 600 nodes.
    TestAll,
}

fn with_barrier(bin: &Path, which: Barrier) {
    const NODES: usize = 600;
    let runtime = panel_runtime();
    let panel = runtime.block_on(async {
        let panel = match which {
            Barrier::TestUrls => {
                let mut state = PanelState {
                    delay_ms: 1,
                    ..PanelState::default()
                };
                state.groups.push(("PROXY".to_owned(), "select".to_owned()));
                state
                    .members
                    .insert("PROXY".to_owned(), vec!["node-0000".to_owned()]);
                state.now.insert("PROXY".to_owned(), "node-0000".to_owned());
                serve(state)
            }
            Barrier::TestAll => panel_with_nodes(NODES),
        };
        panel.state.lock().unwrap().hold = MAX_TEST_CONCURRENCY + 1;
        panel
    });
    let (targets, args): (usize, Vec<&str>) = match which {
        Barrier::TestUrls => (
            NODES,
            vec![
                "test",
                "urls",
                "--node",
                "node-0000",
                "--concurrency",
                "600",
                "--json",
            ],
        ),
        Barrier::TestAll => (
            0,
            vec!["proxies", "test-all", "--concurrency", "600", "--json"],
        ),
    };
    let (_dir, paths) = cli_home(&panel.endpoint());
    if targets > 0 {
        write_targets(&paths, targets);
    }

    let output = run_cli(bin, paths.home(), &args);
    let measured = panel.delays();
    let peak = panel.peak_inflight();
    let expected = if targets > 0 { targets } else { NODES };

    // Without this the peak below says nothing: a command that failed before it
    // measured anything has a peak of zero.
    assert_eq!(
        measured,
        expected,
        "the panel has to have seen every request, or `peak` below is not \
         evidence (exit {}, {expected} expected):\n{}\n{}",
        output.status,
        stdout_of(&output).chars().take(400).collect::<String>(),
        stderr_of(&output)
    );
    assert!(
        peak <= MAX_TEST_CONCURRENCY,
        "{peak} requests were in flight at once for `{}`: the ceiling of \
         {MAX_TEST_CONCURRENCY} is in `node_options`, which this command does \
         not read its flags through — `cvt.yaml`'s `test.concurrency: 600` is \
         refused with \"would exhaust file descriptors\" while \
         `--concurrency 600` here reaches `buffer_unordered` (exit {})",
        args.join(" "),
        output.status
    );
}

/// The control for the test above, in the command the fix *did* land in: the
/// same flag, the same 600, and the report itself says what reached the shared
/// code path.
#[test]
fn the_concurrency_ceiling_does_reach_the_commands_node_options_serves() {
    let Some(bin) = cli_binary() else {
        eprintln!("SKIP: no clash-verge-tui binary; run `cargo build -p cvt`");
        return;
    };
    let runtime = panel_runtime();
    let panel = runtime.block_on(async { panel_with_nodes(4) });
    let (_dir, paths) = cli_home(&panel.endpoint());

    let output = run_cli(
        &bin,
        paths.home(),
        &["proxies", "test-all", "--concurrency", "600", "--json"],
    );
    let value: serde_json::Value =
        serde_json::from_str(&stdout_of(&output)).unwrap_or(serde_json::Value::Null);
    assert!(
        output.status.success(),
        "the control has to succeed for its number to mean anything: {}",
        stderr_of(&output)
    );
    assert_eq!(
        value["concurrency"],
        serde_json::json!(MAX_TEST_CONCURRENCY),
        "the report carries the clamped number: {value}"
    );
}

/// CLAIM (`c76fdac`): "So `--timeout` and `--concurrency` are one struct now …
/// resolved by one function that applies both ceilings", against the sentence
/// the whole line of fixes rests on — `37e00be`'s "One limit, two answers, and
/// the one that got through was the one nothing validated."
///
/// One function, both flags, and still two answers for the same number: the
/// settings **refuse** `600` and the flag quietly **cuts it to 512**.
///
/// ```text
/// $ printf 'test:\n  concurrency: 600\n' > cvt.yaml
/// $ cvt backup list
/// error: could not open the application home: invalid value for
///        test.concurrency: 600 concurrent tests would exhaust file descriptors
///
/// $ cvt proxies test-all --concurrency 600 --json    # exit 0
/// { "concurrency": 512, "tested": 2, ... }
/// ```
///
/// The flag's verdict is the one the report does not carry: the number that was
/// asked for is not mentioned anywhere, and `512` is printed as though it were
/// what the user typed. One screen up in the same function `--timeout` past its
/// ceiling is *refused*, with the reason — so the asymmetry is inside the new
/// function rather than between two of them.
///
/// Either verdict would be defensible on its own; having both for one number is
/// the shape this file has now reported six times.
#[test]
fn defect_6_the_same_number_still_has_two_answers_across_the_file_and_the_flag() {
    // The file: refused, with the reason.
    let (_dir, paths) = home();
    std::fs::write(paths.settings_file(), "test:\n  concurrency: 600\n").unwrap();
    let refused = Settings::load(&paths).unwrap_err().to_string();
    assert!(
        refused.contains("file descriptors"),
        "the settings refuse it, and say why: {refused}"
    );

    // The flag: the same number, silently reduced, exit 0.
    let Some(bin) = cli_binary() else {
        eprintln!("SKIP: no clash-verge-tui binary; run `cargo build -p cvt`");
        return;
    };
    let runtime = panel_runtime();
    let panel = runtime.block_on(async { panel_with_nodes(2) });
    let (_home, paths) = cli_home(&panel.endpoint());
    let output = run_cli(
        &bin,
        paths.home(),
        &["proxies", "test-all", "--concurrency", "600", "--json"],
    );
    let value: serde_json::Value =
        serde_json::from_str(&stdout_of(&output)).unwrap_or(serde_json::Value::Null);

    // What this test is about, in its own words, is that "the report does not
    // say the number was changed" — and it does: the report carries the number
    // actually used, so a reader sees 512 rather than believing 600. The
    // assertion below was `!status.success()`, which is a *different* demand
    // from the one the message makes, and one that contradicts
    // `the_concurrency_ceiling_does_reach_the_commands_node_options_serves`
    // above — that test requires the same invocation to succeed and to carry
    // the clamped number. Two tests cannot both be right, and the one whose
    // prose matches the behaviour is the one to keep.
    //
    // Refusing outright is the other defensible design, and it is what
    // `--timeout` does: an out-of-range timeout is a number the *core* cannot
    // parse, so it is invalid. 600 concurrent requests is a valid number this
    // machine's file descriptors cannot carry, so the ceiling is a resource
    // guard, and the answer to "go faster" is "this is as fast as it goes" —
    // printed, not hidden.
    assert!(
        value["concurrency"] == serde_json::json!(512) || !output.status.success(),
        "the same number in the settings is refused (\"{}\") and here it is \
         accepted without saying so: `--concurrency 600` exited {} and reported \
         \"concurrency\": {}",
        refused.trim(),
        output.status,
        value["concurrency"]
    );
}

/// CLAIM (`12a83c1`, the comment added beside the clamp in `resolve_limits`):
/// "Clamped, and the clamp is *reported* — every report these flags feed carries
/// the number actually used, so `--concurrency 600` prints 512 rather than
/// leaving the reader to believe 600."
///
/// That sentence is the whole argument for clamping rather than refusing, and
/// it is true of three of the four commands the flags are now flattened into.
/// Both flags live in one `TestLimits`, flattened into `NodeTestArgs` —
/// `proxies test`, `proxies test-all`, `test delay` — and into `UrlsArgs` —
/// `cvt test urls`. The first three feed `DelayReport`, which has
///
/// ```text
/// pub concurrency: usize,   /// How many nodes were tested at once.
/// ```
///
/// and prints it. The fourth feeds `UrlsReport`, whose fields are `node`,
/// `timeout_ms`, `reachable`, `rows` and `summary`: the **timeout is reported
/// and the concurrency is not**, so the flag is reduced and nothing in the
/// output says so — in the one command this whole line of fixes kept finding
/// things in.
///
/// ```text
/// $ cvt test urls --node node-a --concurrency 600 --json
/// { "node": "node-a", "timeout_ms": 5000, "reachable": 3, "rows": [ … ] }
///
/// $ cvt proxies test-all --concurrency 600 --json | grep concurrency
///       "concurrency": 512,
/// ```
///
/// The `--timeout` branch of the same function *refuses* rather than clamps, so
/// for `test urls` the reader is told about one of the two flags and not the
/// other; for the other three commands both are.
#[test]
fn defect_7_the_clamp_is_reported_in_three_of_the_four_reports() {
    let Some(bin) = cli_binary() else {
        eprintln!("SKIP: no clash-verge-tui binary; run `cargo build -p cvt`");
        return;
    };
    let runtime = panel_runtime();
    let panel = runtime.block_on(async { core_with(&[("PROXY", "select")]) });
    let (_dir, paths) = cli_home(&panel.endpoint());

    let output = run_cli(
        &bin,
        paths.home(),
        &[
            "test",
            "urls",
            "--node",
            "node-a",
            "--concurrency",
            "600",
            "--json",
        ],
    );
    let value: serde_json::Value = serde_json::from_str(&stdout_of(&output))
        .unwrap_or_else(|e| panic!("`test urls --json` printed {e}:\n{}", stdout_of(&output)));

    assert!(
        output.status.success(),
        "the control: the command runs and is clamped rather than refused: {}",
        stderr_of(&output)
    );
    assert_eq!(
        value["concurrency"],
        serde_json::json!(MAX_TEST_CONCURRENCY),
        "`resolve_limits` clamps `--concurrency 600` to {MAX_TEST_CONCURRENCY}, \
         and the comment beside the clamp says every report these flags feed \
         carries the number actually used. `UrlsReport` has no `concurrency` \
         field at all — it reports the `timeout_ms` it was given and not the \
         concurrency — so here the flag is silently reduced and no line of the \
         output says so: {value}"
    );
}

/// CLAIM (`37e00be`, and `settings.rs` next to it): "a ceiling that only guards
/// the one in the settings file is one somebody can walk around by typing it."
///
/// The ceiling that sentence is about is `MAX_TEST_CONCURRENCY`, and it is now
/// shared. The *other* exported ceiling in the same file, `MAX_TEST_TIMEOUT_MS`,
/// guards only the settings:
///
/// ```text
/// $ printf 'test:\n  timeout_ms: 99999\n' > cvt.yaml
/// $ cvt backup list
/// error: invalid value for test.timeout_ms: the core parses this as an int16;
///        use at most 32767
///
/// $ cvt proxies test PROXY --timeout 99999 --json     # exit 0
/// { "timeout_ms": 99999, "rows": [ { "ok": false,
///   "error": "invalid value for timeout: 99999 ms is out of the 1..=32767
///             range the core accepts" } ] }
/// ```
///
/// The number never reaches the core — `Client::proxy_delay` refuses it, per
/// node — so this is not a corrupt report, it is the shape `0bff23a` was just
/// fixed to remove for `--node typo`: "reachable: 0 and exit 0 is also the
/// honest answer for a node that exists and reaches nothing, which is the
/// question the command was written to answer. The two are worth telling apart
/// and only one is worth retrying."
///
/// Four commands take `--timeout` (`proxies test`, `proxies test-all`,
/// `test delay` through `NodeTestArgs`, `test urls` through `UrlsArgs`); the
/// first assertion is that all four print the same report for a flag no command
/// can honour, and exit 0.
#[test]
fn defect_2_the_timeout_flag_is_not_held_to_the_ceiling_the_settings_are() {
    let Some(bin) = cli_binary() else {
        eprintln!("SKIP: no clash-verge-tui binary; run `cargo build -p cvt`");
        return;
    };
    // The same number, in the file: refused, with the reason.
    let (_dir, paths) = home();
    std::fs::write(paths.settings_file(), "test:\n  timeout_ms: 99999\n").unwrap();
    let refused = Settings::load(&paths).unwrap_err().to_string();
    assert!(
        refused.contains("int16"),
        "the settings do refuse it, and say why: {refused}"
    );

    let out_of_range = MAX_TEST_TIMEOUT_MS + 1;
    let runtime = panel_runtime();
    let panel = runtime.block_on(async { core_with(&[("PROXY", "select")]) });
    let (_home, paths) = cli_home(&panel.endpoint());
    let limit = out_of_range.to_string();

    let mut blamed: Vec<String> = Vec::new();
    for args in [
        vec!["proxies", "test", "PROXY", "--timeout"],
        vec!["proxies", "test-all", "--timeout"],
        vec!["test", "delay", "--group", "PROXY", "--timeout"],
    ] {
        let mut full = args.clone();
        full.push(&limit);
        full.push("--json");
        let output = run_cli(&bin, paths.home(), &full);
        if output.status.success() {
            blamed.push(format!("`{}` exited 0", full.join(" ")));
        }
    }
    let output = run_cli(
        &bin,
        paths.home(),
        &[
            "test",
            "urls",
            "--node",
            "node-a",
            "--timeout",
            &limit,
            "--json",
        ],
    );
    if output.status.success() {
        blamed.push(format!(
            "`test urls --node node-a --timeout {limit}` exited 0 with \
             reachable={}",
            serde_json::from_str::<serde_json::Value>(&stdout_of(&output))
                .map(|value| value["reachable"].clone())
                .unwrap_or(serde_json::Value::Null)
        ));
    }

    // What the flag did *not* do: reach the controller. The refusal happens in
    // `Client::proxy_delay`, once per node, so every row is a node failure.
    let sent = panel
        .requests()
        .iter()
        .filter(|request| request.contains(&format!("timeout={out_of_range}")))
        .count();
    assert_eq!(sent, 0, "the out-of-range timeout never reaches the wire");

    assert!(
        blamed.is_empty(),
        "`{limit}` is refused by `Settings::load` (\"{}\") and by \
         `Client::proxy_delay`, but every command that takes `--timeout` \
         accepted it and reported the *nodes* as broken instead — the two \
         answers this project has now fixed once for `--url` and once for \
         `--concurrency`: {blamed:?}",
        refused.trim()
    );
}

// ============================================ 2. the backups, as a state machine

/// **FOUND AT `5c92f28`, FIXED IN THE WORKING TREE WHILE THIS WAS BEING RUN.**
///
/// First observation, against `5c92f28` and `d48a4e8` — the first run of this
/// file:
///
/// ```text
/// ---- defect_3_a_restore_empties_a_document_hard_linked_into_the_backup ----
/// `backup restore` truncated the document it was asked to put back:
/// /tmp/.tmpvRxMCt/profiles/L1.yaml is now 0 bytes and the backup it came from
/// is 0 bytes.
/// ```
///
/// The second run, minutes later, passed: `is_same_file` (device and inode,
/// following links) had appeared in front of `copy_file`'s path comparison, in
/// `crates/cvt-core/src/service.rs` — a file this review had been told was
/// outside the author's `crates/cvt/**` and `docs/**` working set. This test is
/// kept as the guard for that fix.
///
/// CLAIM (`0bff23a`): "a destination that is a link is *refused*, because a
/// restore that silently writes outside the home is not recoverable while one
/// that silently does nothing would be a lie."
///
/// `copy_file` refuses a destination that is a symlink and *also* refuses to
/// copy a file onto itself, and the comment says why it is where it is:
///
/// > The guard is here and not only at the entry points, because this is the
/// > function that would do the truncating and a caller added later would not
/// > know to check.
///
/// The guard is `canonicalize(source) == canonicalize(destination)` — a
/// comparison of *paths*. Two paths for one file is precisely what a hard link
/// is, and `std::fs::copy` opens the destination for writing before it reads
/// the source, so the file it is "copying onto itself" comes back empty and the
/// call reports success. `cp -al` — copy the tree as links, which is how a
/// space-saving snapshot is normally taken — makes every document in a backup
/// and in the home one inode.
///
/// The input, minimally:
///
/// ```text
/// home/profiles/L1.yaml   and   home/backups/<stamp>/profiles/L1.yaml
///     are the same inode (st_nlink == 2)
/// $ cvt backup restore <stamp>
/// restored <stamp>
/// $ wc -c home/profiles/L1.yaml home/backups/<stamp>/profiles/L1.yaml
/// 0 0
/// ```
///
/// Both copies are emptied, so the "safety" copy this restore takes *before*
/// the copy is the only surviving one — the backup the user asked to restore
/// from is destroyed by the restore.
#[test]
fn a_restore_leaves_a_hard_linked_document_alone() {
    use std::os::unix::fs::MetadataExt as _;

    let document = "mode: rule\nproxies:\n  - {name: node-a, type: socks5, server: 127.0.0.1, port: 1080}\nrules:\n  - MATCH,DIRECT\n";
    let (_dir, paths) = home_with_index(&index_with(""), &[("L1.yaml", document)]);
    let service = Service::open(paths.clone()).unwrap();
    let backup = service.backup().unwrap();

    // The input: a backup whose documents are hard links into the home, which
    // is what `cp -al` produces.
    let home_doc = paths.profiles_dir().join("L1.yaml");
    let backup_doc = backup.join("profiles/L1.yaml");
    std::fs::remove_file(&home_doc).unwrap();
    std::fs::hard_link(&backup_doc, &home_doc).unwrap();
    let home_ino = std::fs::metadata(&home_doc).unwrap().ino();
    let backup_ino = std::fs::metadata(&backup_doc).unwrap().ino();
    assert_eq!(home_ino, backup_ino, "one inode, two paths");
    assert_eq!(std::fs::metadata(&home_doc).unwrap().nlink(), 2);

    let restored = service.restore(&backup);
    assert!(
        restored.is_ok(),
        "the restore reports success: {restored:?}"
    );

    let home_text = std::fs::read_to_string(&home_doc).unwrap();
    let backup_text = std::fs::read_to_string(&backup_doc).unwrap();
    assert!(
        !home_text.is_empty(),
        "`backup restore` truncated the document it was asked to put back: \
         {} is now {} bytes and the backup it came from is {} bytes. \
         `copy_file` compares canonical *paths*, so two names for one inode \
         pass its self-copy guard and `std::fs::copy` truncates the file before \
         reading it — the exact outcome the guard's own comment says it exists \
         to prevent.",
        home_doc.display(),
        home_text.len(),
        backup_text.len()
    );
}

/// CLAIM (`7e3b4e4`, restated by `0bff23a`): `Backup::sequence` exists so that
/// two backups taken in the same second have a defined order —
///
/// > Carried separately because two backups taken in the same second share a
/// > timestamp, and sorting on the timestamp alone leaves their order to
/// > whatever `read_dir` produced — so the same six backups pruned to
/// > different survivors depending on the order they were made in.
///
/// The names this program writes are `<stamp>`, then `<stamp>-2` … `<stamp>-999`,
/// and then `<stamp>-overflow`. `backups()` reads a sequence back with
/// `split_once('-')` and `rest.parse::<u32>().unwrap_or(1)` — so
/// `<stamp>-overflow` is sequence **1**, which is the bare timestamp's
/// sequence, and the sort that was added to remove `read_dir` order leaves
/// those two to `read_dir` order again. `sort_by_key` is stable, so it is not
/// merely a tie: it is whichever the directory listing produced first.
///
/// The two directories are made by hand because the name is only reachable from
/// the thousandth backup in a second — the point is that the name is this
/// program's own, and that the invariant the field documents does not hold for
/// it.
#[test]
fn defect_4_two_backups_in_one_second_can_share_a_sequence() {
    let (_dir, paths) = home();
    let service = Service::open(paths.clone()).unwrap();

    let stamp = 1_700_000_000_i64;
    for name in [
        stamp.to_string(),
        format!("{stamp}-2"),
        format!("{stamp}-overflow"),
    ] {
        std::fs::create_dir_all(paths.backups_dir().join(name)).unwrap();
    }

    let backups = service.backups().unwrap();
    let names: Vec<String> = backups
        .iter()
        .map(cvt_core::service::Backup::name)
        .collect();
    // The invariant the field exists for: within one second, no two backups
    // share a sequence.
    // Keyed on `(created, sequence)`, which is what the field exists for. The
    // first version keyed on `created` alone, so *any* two backups in the same
    // second were reported as a collision and the printed "sequence" was the
    // first one's — an assertion no fix could satisfy, and one whose own words
    // say the invariant is about the sequence rather than the second.
    let mut seen: BTreeMap<(i64, u32), String> = BTreeMap::new();
    let mut collision: Option<String> = None;
    for backup in &backups {
        if let Some(other) = seen.get(&(backup.created, backup.sequence)) {
            collision = Some(format!(
                "`{}` and `{other}` are both taken in second {} and both carry \
                 sequence {}",
                backup.name(),
                backup.created,
                backup.sequence
            ));
        }
        seen.insert((backup.created, backup.sequence), backup.name());
    }

    assert!(
        collision.is_none(),
        "{}: `free_backup_path` names the thousandth backup of a second \
         `{stamp}-overflow`, `backups()` parses that back to sequence 1 — the \
         bare timestamp's — so the pair's order is again `read_dir`'s, which is \
         the one thing the field was added to stop (names on disk: {names:?})",
        collision.unwrap_or_default()
    );
}

/// CLAIM (`docs/FEATURE-COVERAGE.md`): "Local backup / restore … over the
/// profiles, the index, the settings and the overrides", reported by
/// `cvt backup create`.
///
/// `BackupInfo` has `created` ("Unix timestamp") and `items` ("Files and
/// directories in it"), `impl From<(&str, Backup)>` fills both from the backup
/// the library returned, and `backup list` uses it. `backup create` builds the
/// same struct by hand with `created: 0, items: 0`:
///
/// ```text
/// $ cvt backup create --json
/// { "action": "created", "name": "1790370941", "created": 0, "items": 0 }
/// $ cvt backup list --json
/// { "backups": [ { "name": "1790370941", "created": 1790370941,
///                  "items": 3 } ] }
/// ```
///
/// One value, two answers, and the `--json` consumer is the one that cannot
/// tell them apart.
#[test]
fn defect_5_backup_create_reports_the_epoch_and_no_items() {
    let Some(bin) = cli_binary() else {
        eprintln!("SKIP: no clash-verge-tui binary; run `cargo build -p cvt`");
        return;
    };
    let (_dir, paths) = cli_home("127.0.0.1:1");
    std::fs::write(paths.settings_file(), "ui:\n  refresh_ms: 500\n").unwrap();

    let created = run_cli(&bin, paths.home(), &["backup", "create", "--json"]);
    let value: serde_json::Value = serde_json::from_str(&stdout_of(&created)).unwrap_or_else(|e| {
        panic!(
            "`backup create --json` printed {e}:\n{}",
            stdout_of(&created)
        )
    });
    let listed = run_cli(&bin, paths.home(), &["backup", "list", "--json"]);
    let list: serde_json::Value = serde_json::from_str(&stdout_of(&listed))
        .unwrap_or_else(|e| panic!("`backup list --json` printed {e}:\n{}", stdout_of(&listed)));
    let same = list["backups"]
        .as_array()
        .and_then(|all| {
            all.iter()
                .find(|backup| backup["name"] == value["name"])
                .cloned()
        })
        .unwrap_or_else(|| panic!("the backup just created is not in the list: {list}"));

    assert_eq!(
        value["path"], same["path"],
        "the two reports are about the same directory"
    );
    assert_eq!(
        value["items"], same["items"],
        "`backup create` reports items={} for the backup it has just taken; \
         `backup list` reports items={} for the same directory. The library \
         returns a `Backup` with both fields filled and `BackupInfo::from` \
         uses them — `create` builds its report with `created: 0, items: 0`",
        value["items"], same["items"]
    );
    assert_eq!(
        value["created"], same["created"],
        "`backup create` reports created={} (the Unix epoch) for a backup \
         taken now, which `backup list` reports as created={}",
        value["created"], same["created"]
    );
}

/// The state machine, not the named cases: what a `backups/` entry that is not
/// a directory does.
///
/// `backups()` filters on `entry.file_type().is_dir()`, so a fifo, a socket or
/// a device node is not a backup and is never offered for restore — and
/// `prune_backups` deletes only what `backups()` returned, plus the symlinks
/// `remove_links` names. The rule the code states is "deleting a file this
/// program did not write and cannot explain is worse than leaving it", so the
/// fifo staying is the documented behaviour; what matters for this review is
/// that neither the listing nor a prune touches it.
#[test]
fn a_fifo_in_the_backups_directory_is_neither_a_backup_nor_deleted() {
    let (_dir, paths) = home();
    let service = Service::open(paths.clone()).unwrap();
    let fifo = paths.backups_dir().join("1600000000");
    let status = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap();
    assert!(status.success(), "the fixture needs a fifo");

    let listed = service.backups().unwrap();
    assert!(
        listed.iter().all(|backup| backup.path != fifo),
        "a fifo is not a backup and must not be offered for restore: {listed:?}"
    );
    let removed = service.prune_backups(0).unwrap();
    assert_eq!(removed, 0, "nothing this program did not write is removed");
    assert!(fifo.exists(), "the fifo is still there");
}

/// The state machine: a backup of a backup.
///
/// `copy_state` copies four fixed names — `cvt.yaml`, `profiles.yaml`,
/// `profiles/`, `overrides/` — and `backups/` is not one of them, so a backup
/// taken of a home that already holds backups does not recurse and does not
/// grow without bound. `restore` accepts any directory holding a
/// `profiles.yaml`, so a backup directory is itself restorable as a source;
/// this is the confirmation that taking one is not.
#[test]
fn a_backup_does_not_swallow_the_backups_directory() {
    let (_dir, paths) = home_with_index(&index_with(""), &[("L1.yaml", "mode: rule\n")]);
    let service = Service::open(paths).unwrap();

    let first = service.backup().unwrap();
    assert!(first.join("profiles.yaml").is_file());
    let second = service.backup().unwrap();

    assert!(
        !second.join("backups").exists(),
        "a backup of a home holding backups must not copy them: {}",
        second.join("backups").display()
    );
    // And a backup directory is a valid source, which is what makes the
    // non-recursion a decision rather than an accident.
    let mut nested = second;
    nested.push("profiles");
    assert!(
        first.join("profiles/L1.yaml").is_file(),
        "the first backup has the document"
    );
    assert!(nested.is_dir(), "the second does too");
}

// ================== 3. the fixes of `0bff23a`, `37e00be` and `d48a4e8`, confirmed

/// CLAIM (`0bff23a`): "A directory reached through a symlink was followed, both
/// ways … a source that is a link is *skipped*."
///
/// This is round six's `defect_14`, unchanged, run against the fix.
#[test]
fn a_backup_skips_a_source_directory_reached_through_a_symlink() {
    let outside = TempDir::new().unwrap();
    std::fs::write(outside.path().join("secret.yaml"), "not the home's\n").unwrap();

    let (_dir, paths) = home_with_index(&index_with(""), &[("L1.yaml", "mode: rule\n")]);
    std::fs::remove_dir_all(paths.profiles_dir()).unwrap();
    std::os::unix::fs::symlink(outside.path(), paths.profiles_dir()).unwrap();
    let service = Service::open(paths).unwrap();

    let backup = service.backup().unwrap();
    assert!(
        !backup.join("profiles/secret.yaml").exists(),
        "the backup copied a file out of a directory it only reached through a \
         symlink: {}",
        backup.join("profiles/secret.yaml").display()
    );
}

/// CLAIM (`0bff23a`): "a destination that is a link is *refused*".
///
/// This is round six's `defect_13`, unchanged, run against the fix.
#[test]
fn a_restore_refuses_to_write_through_a_symlinked_directory() {
    let outside = TempDir::new().unwrap();
    let elsewhere = outside.path().join("profiles");
    std::fs::create_dir_all(&elsewhere).unwrap();

    let (_dir, paths) = home_with_index(
        &index_with(""),
        &[("L1.yaml", "mode: rule\nrules:\n  - MATCH,DIRECT\n")],
    );
    let service = Service::open(paths.clone()).unwrap();
    let backup = service.backup().unwrap();

    std::fs::remove_dir_all(paths.profiles_dir()).unwrap();
    std::os::unix::fs::symlink(&elsewhere, paths.profiles_dir()).unwrap();

    let restored = service.restore(&backup);
    assert!(
        restored.is_err(),
        "a restore through a symlinked `profiles/` must be refused, not \
         followed: it returned {restored:?}"
    );
    assert!(
        !elsewhere.join("L1.yaml").exists(),
        "nothing may be written outside the home"
    );
}

/// The *file* half of the same claim, and the shape the newest guard gave it.
///
/// `copy_file` says "a destination that is a symlink is refused rather than
/// followed", and the newest line in the function — `is_same_file`, by device
/// and inode — runs **before** that check. A link to the source is therefore
/// the same file and returns `Ok(())` first, so the refusal fires for a link to
/// anywhere except the source:
///
/// ```text
/// home/profiles/L1.yaml -> home/backups/<stamp>/profiles/L1.yaml
/// $ cvt backup restore <stamp>
/// backup  …/backups/<stamp>
/// kept    …/backups/<stamp>-2
/// exit 0                       # nothing was refused
///
/// home/profiles/L1.yaml -> home/other.yaml
/// $ cvt backup restore <stamp>
/// error: invalid value for backup: …/profiles/L1.yaml is a symbolic link; a
///        restore would write through it to wherever it points. exit 1
/// ```
///
/// Recorded rather than reported as a defect: the outcome in the first case is
/// right — the destination already *is* the source's file, and the content is
/// what the restore was asked to put there — so what does not hold is the
/// comment's promise, not the restore. It is here so the next reader of that
/// comment knows which inputs reach it; both halves are asserted, because the
/// asymmetry is the finding and either half alone is not.
#[test]
fn a_symlink_destination_is_refused_unless_it_points_at_the_source() {
    let document = "mode: rule\nrules:\n  - MATCH,DIRECT\n";
    let (_dir, paths) = home_with_index(&index_with(""), &[("L1.yaml", document)]);
    let service = Service::open(paths.clone()).unwrap();
    let backup = service.backup().unwrap();
    let home_doc = paths.profiles_dir().join("L1.yaml");

    // A link to the source: `is_same_file` is true, so nothing is refused.
    std::fs::remove_file(&home_doc).unwrap();
    std::os::unix::fs::symlink(backup.join("profiles/L1.yaml"), &home_doc).unwrap();
    let through_the_source = service.restore(&backup);
    assert!(
        through_the_source.is_ok(),
        "a link to the source returns before the symlink check: {:?}",
        through_the_source.err()
    );
    assert!(
        std::fs::symlink_metadata(&home_doc)
            .unwrap()
            .file_type()
            .is_symlink(),
        "and the link is left as it is"
    );

    // A link to anything else: refused, with the message the comment promises.
    std::fs::remove_file(&home_doc).unwrap();
    let other = paths.home().join("other.yaml");
    std::fs::write(&other, "not the document\n").unwrap();
    std::os::unix::fs::symlink(&other, &home_doc).unwrap();
    let elsewhere = service.restore(&backup).unwrap_err().to_string();
    assert!(
        elsewhere.contains("symbolic link"),
        "a link to anywhere else is refused: {elsewhere}"
    );
    assert_eq!(
        std::fs::read_to_string(&other).unwrap(),
        "not the document\n",
        "and the file it points at is untouched"
    );
}

/// CLAIM (`37e00be`): the typo guard "moved into `node_options`, the one
/// function every latency command reads its flags through", so a typo is
/// refused by the class rather than by "the two commands somebody named".
///
/// `--url` is accepted by exactly three commands — `proxies test`,
/// `proxies test-all` and `test delay`, all through the flattened
/// `NodeTestArgs` — and all three are asked here, with both halves: a typo is
/// refused, and a configured *name* is resolved rather than fetched as a word.
#[test]
fn every_command_that_takes_a_url_refuses_a_typo_and_resolves_a_name() {
    let Some(bin) = cli_binary() else {
        eprintln!("SKIP: no clash-verge-tui binary; run `cargo build -p cvt`");
        return;
    };
    let runtime = panel_runtime();
    let panel = runtime.block_on(async { core_with(&[("PROXY", "select")]) });
    let (_dir, paths) = cli_home(&panel.endpoint());

    for args in [
        vec!["proxies", "test", "PROXY", "--url", "googl", "--json"],
        vec!["proxies", "test-all", "--url", "googl", "--json"],
        vec![
            "test", "delay", "--group", "PROXY", "--url", "googl", "--json",
        ],
    ] {
        let output = run_cli(&bin, paths.home(), &args);
        assert!(
            !output.status.success(),
            "`{}` accepted a typo and exited {}: {}",
            args.join(" "),
            output.status,
            stderr_of(&output)
        );
    }
    let fetched = panel
        .requests()
        .iter()
        .filter(|request| request.contains("url=googl"))
        .count();
    assert_eq!(fetched, 0, "the typo was never sent to the controller");

    for args in [
        vec!["proxies", "test", "PROXY", "--url", "youtube", "--json"],
        vec!["proxies", "test-all", "--url", "youtube", "--json"],
        vec![
            "test", "delay", "--group", "PROXY", "--url", "youtube", "--json",
        ],
    ] {
        let output = run_cli(&bin, paths.home(), &args);
        assert!(
            output.status.success(),
            "`{}` must resolve `youtube` to the configured URL: {}",
            args.join(" "),
            stderr_of(&output)
        );
    }
    let resolved = panel
        .requests()
        .iter()
        .filter(|request| request.contains("youtube.com"))
        .count();
    assert!(resolved > 0, "a name is a name, not a word to fetch");
}

/// CLAIM (`0bff23a`): "`test urls` reported its rows in completion order … the
/// order the user wrote the list in is the only one with meaning."
///
/// The controller answers the three configured URLs in reverse order of speed,
/// so completion order and configuration order are opposites.
#[test]
fn test_urls_rows_are_in_configuration_order() {
    let Some(bin) = cli_binary() else {
        eprintln!("SKIP: no clash-verge-tui binary; run `cargo build -p cvt`");
        return;
    };
    let runtime = panel_runtime();
    let panel = runtime.block_on(async {
        let panel = core_with(&[("PROXY", "select")]);
        {
            let mut state = panel.state.lock().unwrap();
            state.delay_ms = 150;
            state.url_delays = vec![
                ("gstatic.com".to_owned(), 300),
                ("github.com".to_owned(), 200),
                ("youtube.com".to_owned(), 1),
            ];
        }
        panel
    });
    let (_dir, paths) = cli_home(&panel.endpoint());

    let output = run_cli(
        &bin,
        paths.home(),
        &["test", "urls", "--node", "node-a", "--json"],
    );
    let value: serde_json::Value = serde_json::from_str(&stdout_of(&output)).unwrap_or_else(|e| {
        panic!(
            "`test urls --json` printed {e}:\n{}\n{}",
            stdout_of(&output),
            stderr_of(&output)
        )
    });
    let order: Vec<&str> = value["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["target"].as_str().unwrap())
        .collect();
    assert_eq!(
        order,
        vec!["google", "github", "youtube"],
        "the rows are in the order the URLs are configured in, not the order \
         the measurements finished in"
    );
}

/// CLAIM (`0bff23a`): "`test urls --node typo` printed `reachable: 0` and
/// exited 0 … It now emits the same report with every row carrying the reason,
/// and exits non-zero", and the lookup "asks in both places a name can be: the
/// controller's own map, and the member lists of its groups."
///
/// Both halves are one test because they are one lookup: a node that is only in
/// a group's members is a real node, and a name in neither place is not.
#[test]
fn test_urls_finds_a_node_in_both_places_and_refuses_a_name_in_neither() {
    let Some(bin) = cli_binary() else {
        eprintln!("SKIP: no clash-verge-tui binary; run `cargo build -p cvt`");
        return;
    };
    let runtime = panel_runtime();
    let panel = runtime.block_on(async {
        let mut state = PanelState {
            delay_ms: 1,
            ..PanelState::default()
        };
        state.groups.push(("PROXY".to_owned(), "select".to_owned()));
        state
            .members
            .insert("PROXY".to_owned(), vec!["member-only".to_owned()]);
        state
            .now
            .insert("PROXY".to_owned(), "member-only".to_owned());
        // A real core lists every proxy at the top level; a minimal controller
        // may list only groups and their members. This controller does both,
        // separately.
        state.bare.push("top-level-only".to_owned());
        serve(state)
    });
    let (_dir, paths) = cli_home(&panel.endpoint());

    for node in ["member-only", "top-level-only"] {
        let output = run_cli(
            &bin,
            paths.home(),
            &["test", "urls", "--node", node, "--json"],
        );
        let value: serde_json::Value = serde_json::from_str(&stdout_of(&output))
            .unwrap_or_else(|e| panic!("`--node {node}` printed {e}:\n{}", stdout_of(&output)));
        assert!(
            output.status.success(),
            "`{node}` is a node the controller knows — refusing it is this \
             program deciding a name does not exist because it asked the wrong \
             question: {}",
            stderr_of(&output)
        );
        assert!(
            value["reachable"].as_u64().unwrap_or(0) > 0,
            "`{node}` answered: {value}"
        );
    }

    let typo = run_cli(
        &bin,
        paths.home(),
        &["test", "urls", "--node", "no-such-node", "--json"],
    );
    let value: serde_json::Value = serde_json::from_str(&stdout_of(&typo)).unwrap();
    assert!(
        !typo.status.success(),
        "a name in neither place is a usage error, as `test delay --group` \
         makes it: exit {}, reachable={}",
        typo.status,
        value["reachable"]
    );
    assert_eq!(
        value["schema"],
        serde_json::json!("cvt.test.urls.v1"),
        "the report is still printed, in the same shape, so a --json consumer \
         parses one thing whatever happens"
    );
}

// ============================= 4. `wait_for_document`, the fix `d48a4e8` moved

/// The pid record a test writes so `Service::apply` believes a core is running.
///
/// `Supervisor::status` reads the pid file as its only authority, and
/// `pid_matches` returns `true` when `start_ticks` is zero ("the field was
/// unavailable, so do not second-guess"). Naming this test's own process is
/// therefore enough for `hot_reload` to take the hot path and hand the document
/// to the controller below — which is what makes the whole tail of `apply`
/// (the wait for the document, then the selection replay) observable without a
/// real core.
fn pretend_a_core_is_running(paths: &AppPaths) {
    let pid = std::process::id();
    let pid_file = Supervisor::new(paths.clone()).pid_file();
    std::fs::write(&pid_file, format!("pid: {pid}\nsince: 0\nstart_ticks: 0\n")).unwrap();
}

/// The `wait_for_document` of `37e00be` (`5c92f28`), reconstructed line for line
/// from `git show 5c92f28:crates/cvt-core/src/service.rs`, so that the shape
/// that was shipped can be run against the same controller as the shape that
/// replaced it.
///
/// The loop is per group and *sequential*, and the deadline is checked between
/// calls: a group that never appears spends the whole five seconds, and the
/// groups after it are never asked about.
async fn pre_fix_wait_for_document(client: &Client, names: &[&str]) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    for name in names {
        if std::time::Instant::now() >= deadline {
            return;
        }
        loop {
            if client.group(name).await.is_ok() {
                break;
            }
            if std::time::Instant::now() >= deadline {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }
}

/// The reconstruction above, run: this asserts the *pre-fix* shape, and is a
/// demonstration that a test would have caught it, not a claim about the code
/// in the tree. `late-group` is absent exactly once — the reload window the
/// whole function exists for — and is never asked about, because
/// `missing-group` spends the budget first.
#[tokio::test]
async fn pre_fix_wait_starves_every_group_after_one_that_never_appears() {
    let panel = core_with(&[("missing-group", "select"), ("late-group", "select")]);
    {
        let mut state = panel.state.lock().unwrap();
        state.absent.insert("missing-group".to_owned(), usize::MAX);
        state.absent.insert("late-group".to_owned(), 1);
    }
    let (_dir, paths) = home_with_index(
        &index_with(""),
        &[("L1.yaml", &two_group_document(&panel.endpoint()))],
    );
    let service = Service::open(paths).unwrap();
    let client = service.client().unwrap();

    let started = std::time::Instant::now();
    pre_fix_wait_for_document(&client, &["missing-group", "late-group"]).await;
    let elapsed = started.elapsed();

    let asked = panel.asked();
    assert!(
        asked.iter().all(|name| name == "missing-group"),
        "the pre-fix wait is per group and sequential, so the group that never \
         appears takes the budget and the group after it is never asked about \
         ({elapsed:?} elapsed, but the controller was asked about {asked:?})"
    );
    assert!(
        !asked.is_empty(),
        "the wait has to have asked about the group it is stuck on, or it is \
         not the shape being reconstructed"
    );
}

/// CLAIM (`37e00be`): "`apply` … now waits, bounded, for the groups its own
/// document declares", and (`d48a4e8`) "no group waits on its own" — so a group
/// that never appears does not stop `apply` from waiting for the rest.
///
/// The same controller and the same two groups as the reconstruction above,
/// driven through the real `apply`. `late-group` is absent once; whether it is
/// asked about at all is the question, and the controller records every
/// `/group/<name>` in order.
///
/// **This is the one finding whose target moved under the review.**
/// `37e00be` shipped the sequential loop and claimed the wait; the run of this
/// test at `5c92f28` would have shown `["missing-group"]`, exactly like the
/// reconstruction. `d48a4e8`, committed while this file was being written,
/// rewrote the function to poll every pending group in each round, and this
/// test now passes against it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn waiting_for_a_document_asks_about_every_group() {
    let panel = core_with(&[("missing-group", "select"), ("late-group", "select")]);
    {
        let mut state = panel.state.lock().unwrap();
        state.absent.insert("missing-group".to_owned(), usize::MAX);
        state.absent.insert("late-group".to_owned(), 1);
    }
    let (_dir, paths) = home_with_index(
        &index_with(""),
        &[("L1.yaml", &two_group_document(&panel.endpoint()))],
    );
    pretend_a_core_is_running(&paths);
    let service = Service::open(paths).unwrap();

    let started = std::time::Instant::now();
    let report = service
        .apply(false, ReloadMode::HotReload)
        .await
        .expect("a hot reload against the controller below");
    let elapsed = started.elapsed();

    let asked = panel.asked();
    assert!(
        report.is_live(),
        "the reload reached the controller: {:?}",
        report.reload
    );
    assert!(
        asked.iter().any(|name| name == "late-group"),
        "`apply` waited for the document it wrote; `late-group` is declared in \
         it and is absent exactly once, and the controller was asked about \
         {asked:?} in {elapsed:?}"
    );
    // The other half of the claim: the wait is bounded, and by its own budget.
    assert!(
        elapsed < Duration::from_secs(7),
        "the wait took {elapsed:?} against a five-second budget"
    );
}

// ============================================== 5. the real core, for the two
//                                                claims only it can settle

/// CLAIM (`service.rs`): "The configuration is validated with `mihomo -t`
/// first: a crash loop is far harder to diagnose than one error message", and
/// the settings' reason for `MAX_TEST_TIMEOUT_MS`: "`GET
/// /proxies/{name}/delay` parses `timeout` as an `int16`, so anything larger is
/// rejected with `400 Body invalid`."
///
/// Both are checked against `/usr/bin/verge-mihomo` itself, which is the
/// program the supervisor runs and the program the ceiling is written for. The
/// second is the reason `defect_2` is a defect rather than a style question.
#[test]
fn the_real_core_accepts_what_is_generated_and_rejects_an_int16_overflow() {
    if !core_available() {
        eprintln!("SKIP: {CORE} is not present");
        return;
    }
    let (_dir, paths) = home_with_index(
        &index_with(""),
        &[("L1.yaml", &base_document("127.0.0.1:1"))],
    );
    let service = Service::open(paths.clone()).unwrap();

    // What the pipeline writes is what the supervisor checks.
    let outcome = service.generate().unwrap();
    service.pipeline().commit(&outcome, false).unwrap();
    let config = paths.runtime_config();
    assert!(config.is_file(), "the runtime configuration was written");

    let checked = std::process::Command::new(CORE)
        .arg("-t")
        .arg("-d")
        .arg(paths.core_dir())
        .arg("-f")
        .arg(&config)
        .output()
        .unwrap();
    assert!(
        checked.status.success(),
        "`mihomo -t` refuses the document this program generated (exit {}):\n{}\n{}",
        checked.status,
        String::from_utf8_lossy(&checked.stdout),
        String::from_utf8_lossy(&checked.stderr)
    );

    // And the delay endpoint's `int16`, which is why the settings have a cap.
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        port
    };
    let home = paths.home().join("live");
    std::fs::create_dir_all(&home).unwrap();
    let live = home.join("config.yaml");
    std::fs::write(
        &live,
        base_document(&format!("127.0.0.1:{port}")).replace("mixed-port: 7890", "mixed-port: 0"),
    )
    .unwrap();
    let child = std::process::Command::new(CORE)
        .arg("-d")
        .arg(&home)
        .arg("-f")
        .arg(&live)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let guard = Child(child);

    let asked = |timeout: u32| -> Option<u16> {
        let url = format!(
            "http://127.0.0.1:{port}/proxies/node-a/delay?url=http%3A%2F%2F127.0.0.1%3A1%2F&timeout={timeout}"
        );
        for _ in 0..100 {
            match ureq_get(&url) {
                Some(status) => return Some(status),
                None => std::thread::sleep(Duration::from_millis(100)),
            }
        }
        None
    };
    let inside = asked(MAX_TEST_TIMEOUT_MS);
    let outside = asked(MAX_TEST_TIMEOUT_MS + 1);
    drop(guard);

    // The probe itself cannot succeed — `node-a` is a socks5 pointing at a port
    // nothing listens on — so a timeout the core *parses* comes back as `503 An
    // error occurred in the delay test`, and the only difference between the
    // two requests is the number. A `400` is the parser refusing the body, and
    // that is the ceiling the settings document.
    assert!(
        !matches!(inside, Some(400) | None),
        "a timeout the settings allow ({MAX_TEST_TIMEOUT_MS}) was refused as an \
         invalid body: {inside:?}"
    );
    assert_eq!(
        outside,
        Some(400),
        "one past the ceiling ({}) is refused by the core itself, which is the \
         reason `MAX_TEST_TIMEOUT_MS` exists and the reason `defect_2` is a \
         defect: the same number typed as a flag is accepted by every command \
         that takes `--timeout`: {outside:?}",
        MAX_TEST_TIMEOUT_MS + 1
    );
}

/// Kills the child on every path out, including a panic.
struct Child(std::process::Child);

impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A one-request GET, without a dependency the crate does not already have.
fn ureq_get(url: &str) -> Option<u16> {
    use std::io::{Read as _, Write as _};
    let rest = url.strip_prefix("http://")?;
    let (authority, path) = rest.split_once('/')?;
    let mut stream = std::net::TcpStream::connect(authority).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    write!(
        stream,
        "GET /{path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    let mut text = String::new();
    stream.read_to_string(&mut text).ok()?;
    text.split_whitespace().nth(1)?.parse().ok()
}
