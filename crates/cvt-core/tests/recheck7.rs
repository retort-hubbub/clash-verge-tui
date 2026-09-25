//! An **eighth** independent pass, aimed at the backup/restore state machine,
//! the loops that call the controller, the two commands that read other
//! people's HTTP responses, the diagnostic-code scan, and the newest code in
//! the working tree.
//!
//! Every claim below was settled by *running* something where a run was
//! possible. Two of them could not be settled end to end and say so: a test in
//! one crate cannot reach a private function of another, and the two new
//! commands' probe URLs are compiled in, so their readings were attacked
//! through the library the readings are built on (the same `reqwest` the
//! commands use) plus the source that maps a status to a verdict.
//!
//! Method, in the order the review was asked to attack:
//!
//! * the backup/restore state machine as a *whole*, not at the cases the last
//!   two rounds named: a destination that is a second name for a file, a
//!   destination that is a fifo, a restore that cannot finish, two backups at
//!   once, a home with no index at all, and adversarial names;
//! * **every** loop that calls the controller, mechanically, for the pair of
//!   mistakes the last four rounds kept finding (a deadline checked *between*
//!   calls, and a per-item budget that starves what comes after it);
//! * **every** `--timeout` the CLI accepts, taken one at a time, for the
//!   member-versus-class shape this project has fixed six times;
//! * the two new commands' *reading* of other people's responses;
//! * the diagnostic-code scan, attacked as a mechanism rather than as a list.
//!
//! Tests named `defect_*` assert what the code *claims* and are expected to
//! **fail**. A failing `defect_` test is a finding, not a broken test. Tests
//! named `pre_fix_*` assert a shape that was never in the tree, on a
//! reconstruction, and exist to show a test would have caught the defect.
//!
//! Take at `fbf3d85`, with the working tree one commit dirty in
//! `crates/cvt-core/src/profile/store.rs`, `crates/cvt/src/commands/profiles.rs`,
//! `crates/cvt/src/cli.rs`, `crates/cvt-core/tests/recheck6.rs` and `docs/**`.
//! Everything this file attacks in `cvt-core` is at `fbf3d85` and unchanged by
//! that dirt, except `store.rs`, which gained `set_url` — and `set_url` is
//! attacked below *because* it is new.
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
    clippy::field_reassign_with_default
)]

use std::collections::BTreeSet;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::time::{Duration, Instant};

use cvt_core::profile::{MAX_BODY_BYTES, SubscriptionFetcher};
use cvt_core::{AppPaths, Service};
use tempfile::TempDir;

// ==================================================================== fixtures

/// A fresh home with every directory the path helpers expect.
fn home() -> (TempDir, AppPaths) {
    let dir = TempDir::new().unwrap();
    let paths = AppPaths::new(dir.path());
    paths.ensure_dirs().unwrap();
    (dir, paths)
}

/// An index naming one local profile called `L1`, holding `body`.
fn index_one(body: &str) -> (TempDir, AppPaths) {
    let (dir, paths) = home();
    std::fs::write(
        paths.profiles_index(),
        "current: L1\nitems:\n  - uid: L1\n    type: local\n    name: base\n    file: L1.yaml\n",
    )
    .unwrap();
    std::fs::write(paths.profiles_dir().join("L1.yaml"), body).unwrap();
    (dir, paths)
}

/// A home with every kind of state a backup carries.
fn inhabited_home() -> (TempDir, AppPaths) {
    let (dir, paths) = index_one("mode: rule\n");
    std::fs::write(paths.settings_file(), "ui:\n  refresh_ms: 1500\n").unwrap();
    std::fs::write(paths.overrides_dir().join("o.yaml"), "log-level: info\n").unwrap();
    (dir, paths)
}

/// A document declaring a controller, in the shape `Service::endpoint` wants.
fn document(endpoint: &str) -> String {
    format!(
        "mixed-port: 7890\nexternal-controller: {endpoint}\nmode: rule\n\
         proxies:\n  - {{name: node-a, type: socks5, server: 127.0.0.1, port: 1080}}\n\
         proxy-groups:\n  - {{name: PROXY, type: select, proxies: [node-a, DIRECT]}}\n\
         rules:\n  - MATCH,PROXY\n"
    )
}

fn contents(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

/// A listener that accepts connections and never answers a byte.
///
/// The connections are *held*: dropping them would answer with a reset, which
/// is an answer.
fn black_hole() -> (u16, Arc<AtomicUsize>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let accepted = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&accepted);
    std::thread::spawn(move || {
        // The collection *is* the point: it holds every connection open, and
        // dropping one would answer with a reset, which is an answer.
        let _held: Vec<std::net::TcpStream> = listener
            .incoming()
            .filter_map(|stream| {
                counter.fetch_add(1, Ordering::SeqCst);
                stream.ok()
            })
            .collect();
    });
    (port, accepted)
}

/// A port nobody is listening on.
fn closed_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

// ====================================================== a tiny answering server

/// The login wall a redirect can land on, which is what the whole `unlock`
/// reading has to tell apart from a service that operates.
const LOGIN_PAGE: &str = "<!doctype html><html><head><title>Sign in</title></head>\
                          <body><h1>Sign in to continue</h1></body></html>";

/// A one-request-per-connection HTTP server, for the client-side readings.
#[derive(Debug)]
struct Server {
    port: u16,
    asked: Arc<Mutex<Vec<String>>>,
}

impl Server {
    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        302 => "Found",
        404 => "Not Found",
        _ => "Error",
    }
}

fn serve() -> Server {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let asked = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&asked);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let sink = Arc::clone(&sink);
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let read = stream.read(&mut buf).unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..read]).into_owned();
                let path = head
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .split(' ')
                    .nth(1)
                    .unwrap_or("/")
                    .to_owned();
                sink.lock().unwrap().push(path.clone());
                let (status, extra, body): (u16, &str, Vec<u8>) = match path.as_str() {
                    // A page well past the bound the subscription path enforces.
                    "/huge" => (
                        200,
                        "Content-Type: text/plain\r\n",
                        vec![b'a'; MAX_BODY_BYTES + 4096],
                    ),
                    // A service that redirects a probe to its login wall.
                    "/redirect" => (302, "Location: /login\r\n", Vec::new()),
                    "/login" => (
                        200,
                        "Content-Type: text/html; charset=utf-8\r\n",
                        LOGIN_PAGE.as_bytes().to_vec(),
                    ),
                    // A 200 that is an error page, in the shape services use.
                    "/error-json" => (
                        200,
                        "Content-Type: application/json\r\n",
                        br#"{"status":"error","message":"rate limited"}"#.to_vec(),
                    ),
                    // A subscription the new URL serves.
                    other if other.starts_with("/sub") => (
                        200,
                        "Content-Type: text/yaml; charset=utf-8\r\n",
                        subscription().into_bytes(),
                    ),
                    _ => (404, "Content-Type: text/plain\r\n", b"nope".to_vec()),
                };
                let head = format!(
                    "HTTP/1.1 {status} {}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n",
                    reason(status),
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
                let _ = stream.flush();
            });
        }
    });
    Server { port, asked }
}

/// The client `cvt`'s `proxied_client(None, _)` builds: a timeout, a user
/// agent, and nothing else. Every reading below is taken through this.
fn probe_client(timeout: Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(timeout)
        .user_agent(concat!("clash-verge-tui/", env!("CARGO_PKG_VERSION")))
        .build()
        .unwrap()
}

// ===================================================================== the CLI

/// The CLI this project ships, next to the test binary.
fn cli_binary() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.parent()?;
    ["clash-verge-tui", "cvt"]
        .iter()
        .map(|name| dir.join(name))
        .find(|candidate| candidate.is_file())
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

// ==============================================================================
// 1. The backup/restore state machine, attacked as a whole
// ==============================================================================

/// CLAIM (`copy_file`'s doc, `0bff23a`/`c76fdac`): "A destination that is a
/// *symlink* is refused rather than followed … The link is the user's
/// arrangement and is not this function's to replace, so it says so and stops."
///
/// The *class* is "a destination that is a name for something this function did
/// not create", and a symlink is one member of it. The other member is a **hard
/// link**, and it is not refused: `is_same_file(source, destination)` compares
/// the source with the destination, which catches the case round 7 found (one
/// file with two names, both being copied onto each other), and says nothing
/// about a destination that has *other* names of its own.
///
/// So a restore writes into the shared inode and the file outside the home
/// changes underneath it — the same escape the symlink guard exists to stop,
/// reached by the other mechanism, with no error and no mention.
///
/// Observed: the symlink arrangement is refused with "is a symbolic link", and
/// `precious.yaml` outside the home still reads `PRECIOUS\n`; the hard-link
/// arrangement *succeeds* and `precious.yaml` reads `mode: rule\n`.
#[cfg(unix)]
#[test]
fn defect_1_a_restore_writes_through_a_hard_linked_destination() {
    let outside = TempDir::new().unwrap();
    let precious = outside.path().join("precious.yaml");
    std::fs::write(&precious, "PRECIOUS\n").unwrap();

    // The member that *is* guarded: the same arrangement as a symlink.
    let (_d1, paths1) = inhabited_home();
    let service1 = Service::open(paths1.clone()).unwrap();
    let taken1 = service1.backup().unwrap();
    std::fs::remove_file(paths1.profiles_dir().join("L1.yaml")).unwrap();
    std::os::unix::fs::symlink(&precious, paths1.profiles_dir().join("L1.yaml")).unwrap();
    let refused = service1.restore(&taken1).unwrap_err().to_string();
    assert!(refused.contains("symbolic link"), "{refused}");
    assert_eq!(
        contents(&precious),
        "PRECIOUS\n",
        "the symlink guard does hold"
    );

    // The member that is not: a hard link, which is the same thing without a
    // `symlink_metadata` to notice.
    let (_d2, paths2) = inhabited_home();
    let service2 = Service::open(paths2.clone()).unwrap();
    let taken2 = service2.backup().unwrap();
    std::fs::write(paths2.profiles_dir().join("L1.yaml"), "LIVE\n").unwrap();
    std::fs::remove_file(paths2.profiles_dir().join("L1.yaml")).unwrap();
    std::fs::hard_link(&precious, paths2.profiles_dir().join("L1.yaml")).unwrap();

    let restored = service2.restore(&taken2);
    assert!(
        restored.is_ok(),
        "the hard-linked destination is not refused: {}",
        restored.unwrap_err()
    );
    assert_eq!(
        contents(&precious),
        "PRECIOUS\n",
        "a destination that is a second name for a file outside the home is a \
         destination this function did not create; the symlink guard refuses \
         exactly this escape and this one is not noticed. `std::fs::copy` \
         truncates the inode, so the restore edited a file outside the home \
         and reported success"
    );
}

/// CLAIM (`restore`'s doc): "Put a backup back … The state being replaced is
/// copied to a fresh backup first, so restoring the wrong one is itself
/// undoable."
///
/// The identity check is `from.join("profiles.yaml").is_file()`, with
/// "it is not a backup of this home" as the message. A home that has settings
/// worth restoring but no profile index yet — a fresh install that has been
/// configured and not yet subscribed — has no `profiles.yaml`, so the backup
/// `Service::backup` takes of it is one this program then refuses to restore.
///
/// Observed: `backup()` returns `…/backups/<stamp>` containing `cvt.yaml`, and
/// `restore` on it fails with "holds no profiles.yaml, so it is not a backup of
/// this home".
#[test]
fn defect_2_a_backup_this_program_took_from_an_index_less_home_is_refused() {
    let (_dir, paths) = home();
    std::fs::write(paths.settings_file(), "ui:\n  refresh_ms: 4242\n").unwrap();
    let service = Service::open(paths.clone()).unwrap();

    let taken = service.backup().unwrap();
    assert!(
        taken.join("cvt.yaml").is_file(),
        "the settings are what this backup is for: {}",
        taken.display()
    );
    let listed = service.backups().unwrap();
    assert_eq!(listed.len(), 1, "it is listed as a backup: {listed:?}");

    // The state is lost, which is the case a backup exists for.
    std::fs::write(paths.settings_file(), "ui:\n  refresh_ms: 777\n").unwrap();

    let restored = service.restore(&taken);
    assert!(
        restored.is_ok(),
        "the backup was taken by `backup()` and listed by `backups()`, and \
         `restore` refuses it: {}",
        restored.unwrap_err()
    );
    assert_eq!(
        contents(&paths.settings_file()),
        "ui:\n  refresh_ms: 4242\n"
    );
}

/// CLAIM (`restore`'s doc, and `backup.rs`'s): "A restore is **additive**: it
/// writes back what the backup holds and deletes nothing".
///
/// Additive is a statement about which files are touched, not about what a
/// half-finished restore leaves. `copy_state` writes `cvt.yaml`, then
/// `profiles.yaml`, then `profiles/`, then `overrides/` — and the symlink guard
/// on the *destination* fires in the middle. Whatever was written before the
/// refusal stays written, so a refused restore leaves a home made of two
/// different days, and the only report of it is the error.
///
/// Observed: the restore fails on the symlinked `overrides/` with "is a
/// symbolic link", and by then `cvt.yaml` and `profiles/L1.yaml` are already
/// the backup's.
#[cfg(unix)]
#[test]
fn defect_3_a_refused_restore_has_already_written_half_the_backup() {
    let (_dir, paths) = inhabited_home();
    let service = Service::open(paths.clone()).unwrap();
    let taken = service.backup().unwrap();

    // Everything the backup holds is changed, so the home is distinguishable
    // from the backup.
    std::fs::write(paths.settings_file(), "ui:\n  refresh_ms: 777\n").unwrap();
    std::fs::write(paths.profiles_dir().join("L1.yaml"), "LIVE\n").unwrap();
    std::fs::write(paths.overrides_dir().join("o.yaml"), "live-override\n").unwrap();

    // And the last of the four destinations cannot be written at all.
    let outside = TempDir::new().unwrap();
    std::fs::remove_dir_all(paths.overrides_dir()).unwrap();
    std::os::unix::fs::symlink(outside.path(), paths.overrides_dir()).unwrap();

    let refused = service.restore(&taken);
    let message = refused
        .as_ref()
        .err()
        .map(std::string::ToString::to_string)
        .unwrap_or_default();
    assert!(message.contains("symbolic link"), "{message}");

    assert_eq!(
        contents(&paths.settings_file()),
        "ui:\n  refresh_ms: 777\n",
        "the refusal named `overrides/`, which is the *last* of the four \
         things `copy_state` writes; `cvt.yaml` was already replaced by the \
         time the refusal happened, so the home is now two days at once and \
         nothing says so"
    );
    assert_eq!(
        contents(&paths.profiles_dir().join("L1.yaml")),
        "LIVE\n",
        "and so was the document"
    );
}

/// CLAIM (`backup`'s doc): "a backup taken before every experiment would fill a
/// disk with versions of the same file", and `BACKUP_LIMIT` exists to stop it.
///
/// `free_backup_path` asks `exists()` and then returns; nothing reserves the
/// name, and `copy_state` creates it a moment later. Two `backup()` calls
/// racing in the same second therefore both get `<stamp>`, both write into it,
/// and — worse — each one's prune deletes the other's directory, because a
/// backup being written by another thread is listed by `backups()` and is old
/// enough to be pruned.
///
/// Observed: with 24 threads released together, multiple threads are handed the
/// same directory, and returned paths stop existing before the threads even
/// join.
#[test]
fn defect_4_backups_taken_at_once_share_a_directory_and_delete_each_other() {
    let (_dir, paths) = inhabited_home();
    let service = Service::open(paths).unwrap();

    let threads: usize = 24;
    let rounds: usize = 3;
    let mut duplicate = Vec::new();
    let mut vanished = Vec::new();
    let mut incomplete = Vec::new();
    let mut errors = Vec::new();

    for round in 0..rounds {
        for backup in service.backups().unwrap() {
            let _ = std::fs::remove_dir_all(&backup.path);
        }
        let barrier = Arc::new(Barrier::new(threads));
        let mut handles = Vec::new();
        for _ in 0..threads {
            let service = service.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                service.backup()
            }));
        }
        let mut returned: Vec<PathBuf> = Vec::new();
        for handle in handles {
            match handle.join().unwrap() {
                Ok(path) => returned.push(path),
                // Deliberately not unwrapped: a backup that returns an `Io`
                // error naming *another thread's* directory is this finding,
                // and reporting it as a test bug would hide it.
                Err(error) => errors.push(format!("round {round}: {error}")),
            }
        }
        if returned.is_empty() {
            continue;
        }
        let unique: BTreeSet<&PathBuf> = returned.iter().collect();
        if unique.len() != returned.len() {
            duplicate.push(format!(
                "round {round}: {} of {} threads were handed a directory another \
                 thread was handed as well",
                returned.len() - unique.len(),
                returned.len()
            ));
        }
        let gone = returned
            .iter()
            .filter(|path| !path.join("cvt.yaml").is_file())
            .count();
        if gone > 0 {
            vanished.push(format!(
                "round {round}: {gone} of {} returned paths do not hold the \
                 backup that was just taken",
                returned.len()
            ));
        }
        if returned
            .iter()
            .any(|path| !path.join("profiles").join("L1.yaml").is_file())
        {
            incomplete.push(format!("round {round}: a returned backup is partial"));
        }
    }

    // One assertion, so the report carries every way this went wrong rather
    // than only the first.
    let mut problems = duplicate;
    problems.extend(vanished);
    problems.extend(incomplete);
    problems.extend(errors);
    assert!(
        problems.is_empty(),
        "`backup()` is not reentrant, and nothing says so: `free_backup_path` \
         asks `exists()` and returns without reserving the name, and each \
         `backup()` prunes around its own directory without knowing about the \
         others being written. {} call(s) in {rounds} round(s) of {threads} \
         concurrent backups went wrong:\n{}",
        problems.len(),
        problems.join("\n")
    );
}

/// CLAIM (`copy_dir`'s doc): "Only regular files. A symlink here is not
/// something this program writes, and following one would copy a file from
/// wherever the link points".
///
/// A fifo is not a symlink, and `std::fs::copy` opens the destination for
/// writing before it reads the source. Opening a fifo for writing blocks until
/// a reader opens it, and nothing here reads it — so a restore of a home whose
/// document is a fifo does not fail, does not say anything, and never returns.
/// The whole restore hangs, which for `cvt backup restore` is a command that
/// has to be killed, in the middle of the safety copy that was supposed to make
/// the operation undoable.
///
/// Observed: no answer after 5 s (the restore is still blocked, and stays
/// blocked).
#[cfg(unix)]
#[test]
fn defect_5_a_fifo_where_a_document_goes_makes_a_restore_never_return() {
    let (_dir, paths) = inhabited_home();
    let service = Service::open(paths.clone()).unwrap();
    let taken = service.backup().unwrap();

    let fifo = paths.profiles_dir().join("L1.yaml");
    std::fs::remove_file(&fifo).unwrap();
    let status = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap();
    assert!(status.success(), "mkfifo {fifo:?}");

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(service.restore(&taken).map_err(|e| e.to_string()));
    });

    let Ok(returned) = rx.recv_timeout(Duration::from_secs(5)) else {
        panic!(
            "the restore had not returned after 5 s: `std::fs::copy` opens the \
             destination before it reads the source, and a fifo with no reader \
             blocks that open forever. Nothing refuses it — the destination is \
             not a symlink — so `cvt backup restore` is a hung process with no \
             output"
        );
    };
    // The tree moved during this round and this one was fixed while the file
    // was being written: `check_destination` now refuses a destination that is
    // not a regular file, so the restore errors instead of blocking. Recorded
    // rather than asserted away, because at `fbf3d85` it blocked (observed).
    eprintln!("the restore returned: {returned:?}");
    assert!(
        returned.is_err(),
        "a restore cannot have written into a fifo: {returned:?}"
    );
}

/// CLAIM (`Backup.sequence`'s doc): "Carried separately because two backups
/// taken in the same second share a timestamp, and sorting on the timestamp
/// alone leaves their order to whatever `read_dir` produced — so the same six
/// backups pruned to different survivors depending on the order they were made
/// in."
///
/// The suffix is parsed as `u32`, and anything that is not a `u32` becomes
/// `u32::MAX`, so several *different* names still share one `(created,
/// sequence)` pair — `-4294967295`, `-junk` and `-4294967296` are one key. When
/// a tie straddles the keep/cut line, which of them survives is again
/// `read_dir`'s order, which is the thing the field was added to stop.
///
/// Observed: the same six names, created in the opposite order, prune to
/// different survivors (`2000-4294967296` and `2000-junk` one way,
/// `2000-4294967295` and `2000-junk` the other).
#[test]
fn defect_6_adversarial_names_still_tie_so_the_survivor_is_read_dir_order() {
    let names = [
        "2000-4294967295",
        "2000-junk",
        "2000-4294967296",
        "1999",
        "1998",
        "1997",
    ];

    let survivors = |order: &[&str]| -> Vec<String> {
        let (dir, paths) = home();
        for name in order {
            std::fs::create_dir(paths.backups_dir().join(name)).unwrap();
        }
        let service = Service::open(paths.clone()).unwrap();
        service.prune_backups(2).unwrap();
        let mut left: Vec<String> = std::fs::read_dir(paths.backups_dir())
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        drop(dir);
        left
    };

    let forward = survivors(&names);
    let reversed: Vec<&str> = names.iter().rev().copied().collect();
    let backward = survivors(&reversed);

    assert_eq!(
        forward, backward,
        "the same six backups must prune to the same survivors whatever order \
         the directory lists them in — that is the whole reason `sequence` \
         exists. Three of these names parse to one `(created, sequence)` key, \
         so the tie is back and the survivor is the filesystem's"
    );
}

/// CLAIM (`backups`, as fixed in the working tree during this round): a
/// *suffix* that is not the canonical spelling of a `u32` at least 2 is not a
/// backup at all, so that no two names share one `(created, sequence)`.
///
/// The suffix is canonicalised; the **stamp** is not. `"02000".parse::<i64>()`
/// is 2000 and `"+2000".parse::<i64>()` is 2000, so `2000-2`, `02000-2` and
/// `+2000-2` are three different directory names with one `(created,
/// sequence)`, and when they straddle the keep/cut line the survivor is
/// `read_dir`'s order again — which is what the field was added to remove.
/// Same defect as `defect_6`, one member over: the fix covered the member
/// somebody named.
///
/// Observed: the two names created in opposite orders prune to different
/// survivors.
#[test]
fn defect_16_the_stamp_is_not_canonical_so_two_names_still_tie() {
    let survivors = |order: &[&str]| -> Vec<String> {
        let (dir, paths) = home();
        for name in order {
            std::fs::create_dir(paths.backups_dir().join(name)).unwrap();
        }
        let service = Service::open(paths.clone()).unwrap();
        service.prune_backups(1).unwrap();
        let mut left: Vec<String> = std::fs::read_dir(paths.backups_dir())
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        drop(dir);
        left
    };

    let forward = survivors(&["2000-2", "02000-2"]);
    let backward = survivors(&["02000-2", "2000-2"]);
    assert_eq!(
        forward, backward,
        "`2000-2` and `02000-2` are two names and one `(created, sequence)` — \
         both stamps parse to 2000, and both suffixes are the canonical `2` — \
         so whoever survives the prune is decided by the order the filesystem \
         lists them in, which is the defect `sequence` was added for. \
         Canonicalising the suffix and not the stamp is the member-versus-class \
         shape again"
    );
}

// ==============================================================================
// 2. Every loop that calls the controller, for the pair of mistakes
// ==============================================================================

/// CLAIM (`wait_for_document`'s doc, `d48a4e8`): "**no call is outside the
/// deadline**, and **no group waits on its own**. Both halves matter and both
/// were missing here first." And `wait_until_ready`'s: "[`Error::
/// ControllerUnreachable`] when it never answers within the deadline", with
/// `let deadline = Instant::now() + Duration::from_secs(10)`.
///
/// `wait_until_ready` is the fifth loop that calls the controller, and it has
/// the *first* half of that pair and nothing else: `client.version()` is
/// awaited straight, carries the client's own timeout — `ui.refresh_ms * 5`,
/// which is 30 s at the ceiling and 5 s at the default — and the 10-second
/// deadline is only consulted between calls. So a core that answers the socket
/// and never answers `/version` holds `restart_with_rollback` — and therefore
/// `config generate --apply` — for one whole client timeout past the deadline
/// it promised, and for 30× the settings' refresh interval at the ceiling.
///
/// Observed: `wait_until_ready` returned after **30.2 s** with
/// `ui.refresh_ms = 6000` (a 30 s per-call timeout) and a 10 s deadline.
#[tokio::test]
async fn defect_7_wait_until_ready_cannot_enforce_its_ten_second_deadline() {
    let (port, accepted) = black_hole();
    let (_dir, paths) = index_one(&document(&format!("127.0.0.1:{port}")));
    // `Service::client`'s timeout is `refresh_ms * 5`, so this is 30 s.
    std::fs::write(paths.settings_file(), "ui:\n  refresh_ms: 6000\n").unwrap();
    let service = Service::open(paths).unwrap();

    let started = Instant::now();
    let _ = tokio::time::timeout(Duration::from_secs(60), service.wait_until_ready()).await;
    let elapsed = started.elapsed();

    assert!(
        accepted.load(Ordering::SeqCst) > 0,
        "the controller was never asked: {accepted:?}"
    );
    assert!(
        elapsed <= Duration::from_millis(10_300),
        "`wait_until_ready` promises a 10-second deadline and returned after \
         {elapsed:?}: its call carries the client's own timeout \
         (`ui.refresh_ms * 5`) and the deadline is checked between calls, \
         which is the exact shape `wait_for_document` was rewritten to remove \
         one function up"
    );
}

// ==============================================================================
// 3. Every `--timeout` the CLI accepts, taken one at a time
// ==============================================================================

/// CLAIM (`resolve_limits`' doc): "The one place those two flags are read, so a
/// command cannot take them and skip the checks — which is what happened four
/// times: `--concurrency 600` reached `buffer_unordered` while
/// `test.concurrency: 600` was refused, then `test urls` kept its own copy of
/// the flags and did it again, and `--timeout 32768` was accepted by every
/// command while the settings refused it".
///
/// Enumerated mechanically, `--timeout` still has three members: `TestLimits`
/// (checked by `resolve_limits`), `GeoArgs` (checked nowhere) and `UnlockArgs`
/// (checked nowhere). The last two are separate `Option<u64>` fields that never
/// pass through the one function the checks were moved into.
///
/// The value that makes it observable without a network is `0`, which
/// `resolve_limits` refuses ("is outside the range the core accepts") and which
/// the two new commands hand to `reqwest`'s `.timeout()`, where it expires
/// before a request leaves — so every service is reported as having said
/// nothing, and the machine never opens a socket.
///
/// Observed: `test urls --node node-a --timeout 0` exits 1 with the range
/// message; `unlock --timeout 0` exits 0 with four rows reading "no answer".
#[tokio::test]
async fn defect_8_the_new_commands_skip_the_check_every_other_timeout_flag_takes() {
    // What the value *means* where it lands: the builder `proxied_client`
    // uses, given `Duration::from_millis(0)`, which is what `--timeout 0`
    // arrives as. Observed below: the request fails before a request line
    // reaches a server that is listening and answering.
    let answering = serve();
    let expired = probe_client(Duration::ZERO)
        .get(answering.url("/error-json"))
        .send()
        .await;
    let asked = answering.asked();
    // The premise, and what is left of it. Three versions: "nothing is asked
    // and nothing answered" (false), "the request fails" (false), and "the
    // request goes out" (true — except when it does not).
    //
    // Observed across runs: `asked: ["/error-json"]` with the request
    // succeeding, and `asked: []` with it failing before a request line. What
    // reqwest does with a zero timeout is *timing-dependent*, so any assertion
    // about it is a test that fails occasionally for a reason nobody can fix.
    // The premise is recorded and not asserted; what the finding is about is
    // that the command accepted a value with no meaning, which the assertion
    // below still checks.
    let _ = &asked;
    let _ = expired;

    let Some(bin) = cli_binary() else {
        eprintln!("SKIP: no clash-verge-tui binary; run `cargo build -p cvt`");
        return;
    };
    let (_dir, paths) = home();
    std::fs::write(
        paths.settings_file(),
        "test:\n  urls:\n    - {name: u, url: https://example.com/robots.txt}\n",
    )
    .unwrap();
    // `unlock` reads the port to send through from the generated configuration.
    std::fs::write(
        paths.runtime_config(),
        format!("mixed-port: {}\n", closed_port()),
    )
    .unwrap();

    let sibling = run_cli(
        &bin,
        paths.home(),
        &[
            "test",
            "urls",
            "--node",
            "node-a",
            "--timeout",
            "0",
            "--json",
        ],
    );
    assert!(
        !sibling.status.success(),
        "the sibling command accepts the value the four latency commands refuse"
    );
    let sibling_said = format!("{}{}", stdout_of(&sibling), stderr_of(&sibling));
    assert!(
        sibling_said.contains("outside the range"),
        "`test urls --timeout 0` was refused for the reason the review is \
         about: {sibling_said}"
    );

    let unlock = run_cli(&bin, paths.home(), &["unlock", "--timeout", "0", "--json"]);
    // A refusal is the fix this test asks for, and a refused command prints no
    // report — the sibling above is checked the same way, on stdout *and*
    // stderr together. The first version unwrapped the report unconditionally,
    // so it demanded JSON from one command and not from the other.
    let report: serde_json::Value =
        serde_json::from_str(&stdout_of(&unlock)).unwrap_or(serde_json::Value::Null);
    let rows = report["rows"].as_array().cloned().unwrap_or_default();
    let silent = rows
        .iter()
        .filter(|row| row["verdict"] == serde_json::json!("unknown"))
        .count();

    let nothing_left_the_machine = silent == rows.len() && !rows.is_empty();
    assert!(
        !nothing_left_the_machine,
        "`unlock --timeout 0` was **accepted** and answered: every one of the \
         {} services was reported as having said nothing, for a timeout that \
         expires before a request is sent — the same shape as `--timeout \
         32768`, which this project has already fixed once. `--timeout` has \
         three members and one of them (`TestLimits`, via `resolve_limits`) \
         carries the check; `geo` and `unlock` are `Option<u64>` copies that \
         never reach it. Sibling: exit {}, {}. Unlock: exit {:?}, {}",
        rows.len(),
        sibling.status.code().unwrap_or(-1),
        sibling_said.trim(),
        unlock.status.code(),
        stdout_of(&unlock).trim()
    );
}

/// CLAIM (`unlock`'s module doc): "Every verdict here is a *reading of a public
/// response* … this one is wrong often enough — services change their pages,
/// and a probe URL that worked last month may answer a login wall today".
///
/// A login wall is named as the case a reading has to survive, and the client
/// the probes go through sets a timeout and a user agent and **no redirect
/// policy**, so `reqwest`'s default follows up to ten redirects. The status the
/// reading is handed is therefore the *last* one in the chain: a service that
/// redirects its probe to a login page arrives as `HTTP 200`, and the arm that
/// exists for it — `301 | 302 | 403 => Blocked` for `disney-plus` — cannot be
/// reached that way.
///
/// Settled through the library rather than the command: the probe URLs are
/// compiled in, so the response cannot be injected into the process, and no
/// private function of a binary crate can be called from here. What *is*
/// observable is the client's behaviour (below), and the arm it feeds, quoted
/// from the source.
///
/// Observed: a 302 to `/login` is followed; the reading receives `200` and the
/// login page's own body.
#[tokio::test]
async fn defect_9_a_probe_redirected_to_a_login_wall_is_read_as_unlocked() {
    let server = serve();
    let client = probe_client(Duration::from_secs(10));
    let response = client
        .get(server.url("/redirect"))
        .send()
        .await
        .expect("the redirect is answered");
    let status = response.status().as_u16();
    let body = response.text().await.unwrap();

    assert_eq!(
        status,
        200,
        "a 302 was not reported as a 302: {status}, asked {:?}",
        server.asked()
    );
    assert!(body.contains("Sign in"), "the login page's body arrived");

    let source = include_str!("../../cvt/src/commands/unlock.rs");
    let netflix = source
        .split("\"netflix\" =>")
        .nth(1)
        .unwrap_or_default()
        .split("\"chatgpt\" =>")
        .next()
        .unwrap_or_default();
    let proxied = include_str!("../../cvt/src/commands/mod.rs");
    let builder = proxied
        .split("pub fn proxied_client")
        .nth(1)
        .unwrap_or_default()
        .split("/// The port the core is listening on")
        .next()
        .unwrap_or_default();
    if builder.contains("redirect") {
        eprintln!("the probe client now sets a redirect policy; this finding is fixed");
        return;
    }
    let page = include_str!("../../../docs/CLI.md");
    assert!(
        page.contains("a login wall"),
        "the page promises a login wall is `unknown`"
    );

    assert!(
        !netflix.contains("200 =>"),
        "a login wall arrives as `HTTP 200` (observed above: the 302 was \
         followed, and the reading was handed 200 and the login page's body), \
         and this arm turns *any* 200 into `Unlocked`, so the three services \
         whose signal is the status report a login wall as the service being \
         available. `docs/CLI.md` says the opposite in as many words: \
         \"`unknown` is a real answer here and means the response said nothing \
         either way — a login wall, a rate limit, a redesign\". The arm is:\n\
         {netflix}"
    );
}

/// CLAIM (`unlock::ask`): "Bounded: these are pages, and only a prefix is
/// needed to read a signal."
///
/// There is no bound: the body is `response.text()`, which buffers whatever
/// arrives. The crate *has* a bound — `MAX_BODY_BYTES`, enforced by
/// `read_body`, which exists "precisely so that a provider cannot make the
/// process allocate without bound" — and it is applied to subscriptions
/// (`profile/source.rs`) and to neither of the two commands that read other
/// people's responses. Same client, same response, two verdicts.
///
/// Observed: `text()` returned all 33 554 432 bytes; the same request through
/// `SubscriptionFetcher::fetch_with_client` was refused with "over the
/// 33554432-byte limit".
#[tokio::test]
async fn defect_10_the_bound_on_a_response_body_covers_the_fetcher_and_not_the_probes() {
    let server = serve();
    let client = probe_client(Duration::from_secs(60));
    let url = server.url("/huge");

    // What the new commands do with a response.
    let body = client.get(&url).send().await.unwrap().text().await.unwrap();
    assert_eq!(
        body.len(),
        MAX_BODY_BYTES + 4096,
        "`Response::text()` buffered every byte of a {MAX_BODY_BYTES}-byte-plus \
         body"
    );

    // What the code path that has the bound does with the same one.
    let fetcher = SubscriptionFetcher::new(None).unwrap();
    let refused = fetcher
        .fetch_with_client(&client, &url)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        refused.contains("limit"),
        "the fetcher's bound is the contrast this finding rests on: {refused}"
    );

    let unlock = include_str!("../../cvt/src/commands/unlock.rs");
    let geo = include_str!("../../cvt/src/commands/geo.rs");
    let unbounded = |source: &str| {
        !source.contains("MAX_BODY_BYTES")
            && !source.contains("bytes_stream")
            && !source.contains("content_length")
    };
    assert!(
        !unbounded(unlock) || !unbounded(geo),
        "neither command reaches the bound this crate already has, and \
         `unlock` states a bound it does not have:\n  unlock: `{}`\n  geo: \
         `{}`\nThe same client and the same response are refused by \
         `SubscriptionFetcher` (above), which is where `MAX_BODY_BYTES` lives \
         \"precisely so that a provider cannot make the process allocate \
         without bound\"",
        unlock
            .lines()
            .find(|line| line.contains("Bounded"))
            .unwrap_or("(no claim)"),
        geo.lines()
            .find(|line| line.contains("text()"))
            .unwrap_or("(no read)")
    );
}

/// CLAIM (`geo.rs`'s own unit test): "A shape neither of them uses must not
/// panic, and must not invent an address: **an empty `ip` is the caller's cue
/// to try the next source**."
///
/// The caller does not take the cue. `run` returns on the first `Ok(found)`
/// whatever `found.ip` is, so the *first* source that answers at all — with an
/// error page, a rate-limit notice, a login wall, or any JSON at all, since
/// `from_json` reads nothing it does not recognise and `fetch` only rejects a
/// non-success *status* — ends the search and the report goes out with a blank
/// address, `source: https://ipinfo.io/json`, and exit code 0.
///
/// Settled by inspection, and by the parse below: the reading functions are
/// private to the binary crate and the source URLs are compiled in, so no
/// response can be injected into a running `cvt geo`.
///
/// Observed: `{"status":"error","message":"rate limited"}` parses as JSON, so
/// it takes the `from_json` path, which invents nothing and leaves `ip` empty.
#[test]
fn defect_11_geo_returns_the_first_answer_however_empty_it_is() {
    let body = r#"{"status":"error","message":"rate limited"}"#;
    assert!(
        serde_json::from_str::<serde_json::Value>(body).is_ok(),
        "a 200 error page in JSON is the shape `from_json` is handed"
    );

    let source = include_str!("../../cvt/src/commands/geo.rs");
    let arm = source
        .split("for source in SOURCES")
        .nth(1)
        .unwrap_or_default()
        .split("Err(error) =>")
        .next()
        .unwrap_or_default();
    let page = include_str!("../../../docs/CLI.md");
    assert!(
        page.contains("Three sources are tried in order"),
        "the page promises the fallback this rests on"
    );

    assert!(
        arm.contains("is_empty"),
        "the caller's cue, from the test in this same file — \"an empty `ip` \
         is the caller's cue to try the next source\" — is not taken, and the \
         page promises the same thing: \"Three sources are tried in order — \
         `ipinfo.io`, `ip.sb` and Cloudflare's plain-text trace — because they \
         are other people's services, and one being down or refusing a request \
         from a given country is not a reason for this command to have nothing \
         to say\". A service that answers 200 with a rate-limit notice answers \
         *the first source*, so the other two are never tried and the report \
         goes out with `ip` blank. The whole body of the loop is:\n{arm}"
    );
}

// ==============================================================================
// 4. The diagnostic-code scan, attacked as a mechanism
// ==============================================================================

/// The set `invariants.rs::constructed_codes` reads out of a source string.
///
/// Reconstructed faithfully from `crates/cvt-core/tests/invariants.rs`, so the
/// mechanism can be run against sources that are not the tree's.
/// The scan this pins, copied from `invariants.rs` — over-inclusive, and
/// bounded at the validator's own test module.
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
    found.sort();
    found.dedup();
    found
}

/// `E-SOMETHING`, `W-SOMETHING` or `I-SOMETHING`, in capitals.
fn looks_like_a_code(literal: &str) -> bool {
    let mut chars = literal.chars();
    matches!(chars.next(), Some('E' | 'W' | 'I'))
        && chars.next() == Some('-')
        && literal.len() > 2
        && literal
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '-')
}

/// The codes the diagnostics page documents as *table rows*.
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

/// CLAIM (`invariants.rs`, `every_code_the_validator_produces_is_in_the_table`
/// and `every_code_the_validator_produces_is_documented`): "the set is read out
/// of the source rather than trusted", and "Every code the validator produces
/// must have an entry in the diagnostics page".
///
/// Both checks are textual in a way that is right for the wrong reason. The
/// scan looks for `Diagnostic::<kind>("` and a literal, so a code that reaches
/// the constructor through a `const`, a variable or a `match` is invisible to
/// it — while a code written inside a macro that is never called is *found*.
/// And the documentation check is `page.contains(code)`, which two names in the
/// page already satisfy for the wrong reason: `E-CIDR-FAMILY` and
/// `E-UNREACHABLE-RULES` appear only in the sentences that say they were
/// *removed* ("A code that stops being produced is removed rather than
/// repurposed (`E-CIDR-FAMILY` went that way)"). So the check cannot tell
/// "documented" from "retracted", and a produced code named after a retracted
/// one would pass it with the page telling the reader it does not exist.
///
/// Whether that is reached today: it is not. All 39 codes the scan finds are
/// table rows, which the assertion at the end below confirms — the hole is in
/// the mechanism, and the mechanism is what a new code meets. The retraction
/// half of it is `defect_15`.
///
/// Observed, at the time: the reconstruction found 0 of the 3 codes the
/// synthetic source produces. **Fixed** — `scan_codes` below now reads every
/// code-shaped literal outside the validator's own test module, which finds all
/// three, and the assertion is unchanged. The residual is worth knowing: a code
/// with no literal anywhere — assembled at runtime, or a `const` living in
/// another file — is still invisible, because the scan is still textual.
#[test]
fn defect_12_the_code_scan_cannot_see_a_const() {
    // What the mechanism does with a source that produces three codes.
    let production = r#"
        const NEW_CODE: &str = "E-NEW-CODE";
        fn from_const() { let _ = Diagnostic::error(NEW_CODE, "message"); }
        fn from_match(kind: bool) {
            let code = if kind { "W-OTHER" } else { "E-THIRD" };
            let _ = Diagnostic::error(code, "message");
        }
    "#;
    let seen = scan_codes(production);
    let expected = ["E-NEW-CODE", "W-OTHER", "E-THIRD"];
    let missed: Vec<&str> = expected
        .iter()
        .filter(|code| !seen.iter().any(|seen| seen == *code))
        .copied()
        .collect();
    assert!(
        missed.is_empty(),
        "the completeness check is textual and reads only a literal after \
         `Diagnostic::<kind>(`, so {missed:?} are produced by this source and \
         invisible to it — a `const`, a `match` or a macro parameter is all it \
         takes, and then no test asks whether such a code is reachable or \
         documented. The scan saw {seen:?}"
    );

    // Today the tree is clean under the stronger reading: every code the scan
    // finds is a table row. That is what makes the hole latent rather than
    // active, and it is worth asserting, because a *fix* to the scan must not
    // change it.
    let page = include_str!("../../../docs/DIAGNOSTICS.md");
    let rows = table_codes(page);
    let produced = scan_codes(include_str!("../src/validate.rs"));
    let undocumented: Vec<&String> = produced
        .iter()
        .filter(|code| !rows.contains(&code.as_str()))
        .collect();
    assert!(
        undocumented.is_empty(),
        "the tree is currently clean under this stronger check: {undocumented:?}"
    );
}

/// CLAIM (`invariants.rs::every_code_the_validator_produces_is_documented`):
/// "Every code the validator produces must have an entry in the diagnostics
/// page, and the page must not list a code that no longer exists. A code is
/// what a user searches for … a code with nowhere to look it up is a code that
/// will be read as noise."
///
/// The check asks `page.contains(code)`. Two names in the page satisfy that
/// only through the sentences that say the code was **removed** — line 15's "a
/// code that stops being produced is removed rather than repurposed
/// (`E-CIDR-FAMILY` went that way)" and the closing section's
/// "`E-UNREACHABLE-RULES` was an error until a real core was asked" — and
/// neither is a table row. So the check cannot tell a documented code from a
/// retracted one, and a code named after a retracted one passes it while the
/// page tells the reader the code does not exist. The *opposite* direction is
/// armed too: `table_codes` is what the stale-row check reads, so a code that
/// is only a retraction is invisible to that one as well.
///
/// Observed: `E-CIDR-FAMILY` and `E-UNREACHABLE-RULES` are the only two names
/// accepted by `page.contains` that are not table rows.
#[test]
fn defect_15_the_documentation_check_accepts_a_retraction() {
    let page = include_str!("../../../docs/DIAGNOSTICS.md");
    let rows = table_codes(page);
    let produced = scan_codes(include_str!("../src/validate.rs"));
    let accepted = produced
        .iter()
        .filter(|code| page.contains(code.as_str()))
        .count();
    assert_eq!(
        accepted,
        produced.len(),
        "the premise: every code the scan finds satisfies the check today"
    );

    let retracted: Vec<&str> = ["E-CIDR-FAMILY", "E-UNREACHABLE-RULES"]
        .into_iter()
        .filter(|code| page.contains(code) && !rows.contains(code))
        .collect();
    assert!(
        retracted.is_empty(),
        "{retracted:?} satisfy `every_code_the_validator_produces_is_documented` \
         through the sentences that say the code was **removed**, so that test \
         cannot tell a documented code from a retracted one — and the stale-row \
         check in the same test reads only table rows, so it cannot see them \
         either"
    );
}

// ==============================================================================
// 5. The newest code in the working tree: `profiles edit-url`
// ==============================================================================

/// A subscription document the fetcher will accept.
fn subscription() -> String {
    "mixed-port: 7890\nproxies:\n  - {name: node-a, type: socks5, \
     server: 127.0.0.1, port: 1080}\nrules:\n  - MATCH,DIRECT\n"
        .to_owned()
}

/// CLAIM (`profiles.rs`, `edit_url`): "Change a subscription's URL, and by
/// default fetch from the new one … `--no-fetch` exists for the case where the
/// new provider is not reachable yet, and says what it left behind."
///
/// The URL reaches the index *before* it is fetched, and nothing checks that it
/// can be fetched: a scheme the fetcher refuses (`file://`, which the fetcher
/// answers with "scheme `file` cannot be fetched; use http or https") is written
/// to `profiles.yaml` and only then refused, so the command's failure leaves
/// the profile pointing at something that can never download. The report says
/// the URL changed and the document is the old provider's, which the
/// `--no-fetch` branch is careful to warn about and this branch reaches by
/// accident.
///
/// Observed: `profiles edit-url L1 file:///etc/passwd` writes the URL into the
/// index, fails, exits 1, and the index keeps `file:///etc/passwd`.
#[test]
fn defect_13_a_url_that_cannot_be_fetched_is_persisted_before_it_is_fetched() {
    let Some(bin) = cli_binary() else {
        eprintln!("SKIP: no clash-verge-tui binary; run `cargo build -p cvt`");
        return;
    };
    let (_dir, paths) = index_one("mode: rule\n");
    // A remote profile, with a URL, so `set_url` has something to change.
    std::fs::write(
        paths.profiles_index(),
        "current: R1\nitems:\n  - uid: R1\n    type: remote\n    name: panel\n    \
         url: \"http://127.0.0.1:1/sub\"\n    file: R1.yaml\n",
    )
    .unwrap();
    std::fs::write(paths.profiles_dir().join("R1.yaml"), "mode: rule\n").unwrap();

    let output = run_cli(
        &bin,
        paths.home(),
        &["profiles", "edit-url", "R1", "file:///etc/passwd"],
    );
    assert!(
        !output.status.success(),
        "the fetcher refuses the scheme, so the command does: {}{}",
        stdout_of(&output),
        stderr_of(&output)
    );

    let written = contents(&paths.profiles_index());
    let said = format!("{}{}", stdout_of(&output), stderr_of(&output));
    // The order — persist, then fetch — is `profiles add`'s, and `add` argues
    // for it in as many words ("the profile exists either way … re-adding would
    // lose the name the user chose"). Run against this tree:
    // `profiles add --name x file:///etc/passwd` keeps the URL too, so this is
    // *inherited* rather than invented. What the argument does not cover is a
    // URL the fetcher can never accept: the recovery printed with the refusal
    // is `profiles update`, and for a scheme refused before a socket exists
    // that retry cannot succeed however many times it is run.
    let advised_a_retry = said.contains("retry with `clash-verge-tui profiles update");
    assert!(
        !written.contains("file:///etc/passwd") || !advised_a_retry,
        "the URL was refused by the fetcher, kept by the index, and the advice \
         printed with the refusal is a retry that can never work — `profiles \
         update` runs the same fetcher with the same scheme. Either refuse the \
         scheme before writing it, or say the profile now points at something \
         this program cannot download and how to repoint it:\nindex:\n\
         {written}\nsaid:\n{said}"
    );
}

/// CLAIM (`profiles.rs`, `edit_url`, and `docs/CLI.md`): the human report of a
/// URL change, and the shape a `--json` consumer gets.
///
/// `edit_url` reuses `AddReport` for the fetching path and `ProfileChangeReport`
/// for `--no-fetch`, so one command answers in two different schemas: the
/// machine-readable one says `cvt.profiles.added.v1` and `action: "url
/// changed"`, while the terminal rendering of that same report is hard-coded to
/// print `added`. Nothing was added.
///
/// Observed: `cvt profiles edit-url R1 http://…/sub` prints "added R1 (…)";
/// with `--no-fetch` it prints "url changed `https…` (R1)".
#[tokio::test]
async fn defect_14_a_url_change_is_reported_as_an_addition() {
    let Some(bin) = cli_binary() else {
        eprintln!("SKIP: no clash-verge-tui binary; run `cargo build -p cvt`");
        return;
    };
    let server = serve();
    // A subscription the new URL serves, so the *succeeding* path is the one
    // whose report is examined.
    let (_dir, paths) = home();
    std::fs::write(
        paths.profiles_index(),
        "current: R1\nitems:\n  - uid: R1\n    type: remote\n    name: panel\n    \
         url: \"http://127.0.0.1:1/sub\"\n    file: R1.yaml\n",
    )
    .unwrap();
    std::fs::write(paths.profiles_dir().join("R1.yaml"), subscription()).unwrap();

    let new_url = server.url("/sub");
    let output = run_cli(
        &bin,
        paths.home(),
        &["profiles", "edit-url", "R1", &new_url],
    );
    let text = format!("{}{}", stdout_of(&output), stderr_of(&output));
    let fetched = server.asked();
    assert!(
        output.status.success() && fetched.contains(&"/sub".to_owned()),
        "the new URL was fetched and accepted: exit {:?}, {text}, asked \
         {fetched:?}",
        output.status.code()
    );

    let json = run_cli(
        &bin,
        paths.home(),
        &["profiles", "edit-url", "R1", &server.url("/sub2"), "--json"],
    );
    let report: serde_json::Value = serde_json::from_str(&stdout_of(&json)).unwrap();
    let schema = report["schema"].as_str().unwrap_or("(none)").to_owned();
    let nofetch = run_cli(
        &bin,
        paths.home(),
        &[
            "profiles",
            "edit-url",
            "R1",
            &server.url("/sub3"),
            "--no-fetch",
            "--json",
        ],
    );
    let nofetch_report: serde_json::Value = serde_json::from_str(&stdout_of(&nofetch)).unwrap();
    let nofetch_schema = nofetch_report["schema"]
        .as_str()
        .unwrap_or("(none)")
        .to_owned();

    // One assertion, so both halves of the report's problem are in the output.
    let mut problems = Vec::new();
    if text.lines().any(|line| line.contains("added")) {
        problems.push(format!(
            "the terminal report says the profile was added:\n{text}"
        ));
    }
    if schema != nofetch_schema {
        problems.push(format!(
            "and the machine-readable one answers in two shapes for one \
             command: `{schema}` with a fetch ({report}), `{nofetch_schema}` \
             with `--no-fetch` ({nofetch_report})"
        ));
    }
    assert!(
        problems.is_empty(),
        "`profiles edit-url` changed a URL and reported it as an addition:\n{}",
        problems.join("\n")
    );
}
