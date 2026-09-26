//! A **tenth** independent pass, aimed at the three surfaces the ninth review
//! wrote down as "not attacked", at the interface crate no round has ever
//! pointed at, and at the shape this project has now produced seven times: *a
//! guard that covers the members somebody named rather than the class they
//! belong to*.
//!
//! Method, in the order the review was asked to attack:
//!
//! * the **CLI flag class**, enumerated mechanically rather than read: the
//!   subcommand tree and the flags each command declares are taken from the
//!   binary's own `--help`, so a command that takes `--timeout`,
//!   `--concurrency` or `--url` without the checks shows up as a new member
//!   rather than as a thing nobody looked at;
//! * the **controller-call loops**, attacked as a class: every loop that calls
//!   the client, against a controller that accepts the connection and never
//!   answers a byte — the only hostile shape that tells "bounded by a deadline
//!   around the call" apart from "bounded by a deadline checked between
//!   calls";
//! * the **`backup()` concurrency race** round 8 fixed but never re-ran, run
//!   harder and with the second half of the invariant (the directory a thread
//!   is handed must actually *hold* the backup), plus the state a backup that
//!   *fails* leaves behind;
//! * the **`cvt-tui` crate**, which no review has been pointed at: the
//!   settings screen's edit operations against `Settings::validate` and against
//!   a save/load round trip, and the one route by which disk state replaces
//!   in-memory state.
//!
//! ### Why this file is in `cvt-tui`
//!
//! The round's file is one file. Half of what it attacks is the interface, and
//! an integration test of `cvt-tui` can use `cvt-core` (a dependency of this
//! crate) as well as the crate under test — so the core-facing tests ride along
//! here rather than the interface tests being dropped. `cargo test -p cvt-tui`
//! runs all of them.
//!
//! `defect_*` names preserve the original findings; all tests now must pass.
//! Tests named `confirmed_*` assert a claim that was checked and holds; `observed_*`
//! record a measurement whose verdict is the author's to make.
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
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use cvt_core::enhance::pipeline::Pipeline;
use cvt_core::mihomo::supervisor::CoreStatus;
use cvt_core::profile::store::ProfileStore;
use cvt_core::service::BACKUP_LIMIT;
use cvt_core::{AppPaths, Service, Settings};
use cvt_tui::app::POLL_EVERY;
use cvt_tui::{App, Data, Done, Effect, Event, Overlay, Screen, SettingKind, Theme};
use tempfile::TempDir;

// ==================================================================== helpers

/// The interface, with the given settings already loaded.
///
/// `App::new` starts from `Settings::default()` and the binary answers the
/// first refresh with `Data::Settings`; injecting them is the same path with
/// the read taken out, which is what makes these tests need no disk and no
/// core.
fn app_with(settings: Settings, home: &Path) -> App {
    let mut app = App::new(home.to_path_buf(), Theme::default());
    app.screen = Screen::Settings;
    app.on_event(Event::Data(Data::Settings(Box::new(settings))));
    app
}

/// One key press, with no modifiers.
fn press(app: &mut App, code: KeyCode) -> Vec<Effect> {
    app.on_event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
}

/// What a text setting *is*, as opposed to what its row *shows*.
///
/// Written as a second table on purpose: the class under test is "every row
/// whose kind is [`SettingKind::Text`]", and the tests below assert that this
/// table and that class are the same set — so a sixth text row fails the test
/// with "extend this table" rather than being silently skipped.
fn text_setting(settings: &Settings, key: &str) -> String {
    match key {
        "core.binary" => settings
            .core
            .binary
            .as_ref()
            .map_or_else(|| "(none)".to_owned(), |p| p.display().to_string()),
        "core.external_controller" => settings
            .core
            .external_controller
            .clone()
            .unwrap_or_else(|| "(none)".to_owned()),
        "core.secret" => settings
            .core
            .secret
            .clone()
            .unwrap_or_else(|| "(none)".to_owned()),
        "test.url" => settings.test.url.clone(),
        "test.expected_status" => settings.test.expected_status.clone(),
        other => panic!("`{other}` is a text row this table does not know"),
    }
}

/// Every row the settings screen offers whose kind is `Text`.
fn text_keys(settings: &Settings) -> BTreeSet<&'static str> {
    cvt_tui::app::setting_rows(settings)
        .into_iter()
        .filter(|row| row.kind == SettingKind::Text)
        .map(|row| row.key)
        .collect()
}

/// The value the open prompt was seeded with.
fn seeded(app: &App) -> String {
    match &app.overlay {
        Some(Overlay::Prompt { value, .. }) => value.clone(),
        other => panic!("no prompt is open: {other:?}"),
    }
}

/// A home with a settings file, an index and one base document.
fn inhabited_home() -> (TempDir, AppPaths) {
    let dir = TempDir::new().unwrap();
    let paths = AppPaths::new(dir.path());
    paths.ensure_dirs().unwrap();
    std::fs::write(paths.settings_file(), "ui:\n  refresh_ms: 250\n").unwrap();
    std::fs::write(
        paths.profiles_index(),
        "current: L1\nitems:\n  - uid: L1\n    type: local\n    name: base\n    file: L1.yaml\n",
    )
    .unwrap();
    std::fs::write(paths.profiles_dir().join("L1.yaml"), BASE_DOCUMENT).unwrap();
    std::fs::write(
        paths.overrides_dir().join("keep.yaml"),
        "tun:\n  enable: true\n",
    )
    .unwrap();
    (dir, paths)
}

/// A configuration the generator accepts, so anything it is asked about is the
/// thing under test rather than the document.
const BASE_DOCUMENT: &str = "\
mixed-port: 7890
external-controller: 127.0.0.1:9090
mode: rule
proxies:
  - { name: node-a, type: socks5, server: 127.0.0.1, port: 1080 }
  - { name: node-b, type: socks5, server: 127.0.0.1, port: 1081 }
proxy-groups:
  - { name: PROXY, type: select, proxies: [node-a, node-b, DIRECT] }
rules:
  - MATCH,PROXY
";

/// A listener that accepts connections and never answers a byte.
///
/// The connections are *held*: dropping one would answer with a reset, which is
/// an answer, and an answer is exactly what a deadline test must not provide.
fn black_hole() -> (u16, Arc<AtomicUsize>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let accepted = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&accepted);
    std::thread::spawn(move || {
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

/// The commands that declare `--timeout`, `--concurrency` or `--url`, and the
/// arguments each needs before its flags are read.
///
/// Hand-written because a positional has to be supplied, but *checked* against
/// the binary's own `--help` below: the test derives the class from the binary
/// and fails when a member of it is missing from this table.
const LATENCY_COMMANDS: &[(&str, &[&str])] = &[
    ("geo", &[]),
    ("proxies test", &["PROXY"]),
    ("proxies test-all", &[]),
    ("test delay", &["--group", "PROXY"]),
    ("test urls", &["--node", "node-a"]),
    ("unlock", &[]),
];

/// The three flags whose class this round enumerates.
const LATENCY_FLAGS: &[&str] = &["timeout", "concurrency", "url"];

/// A value of each flag that nothing should accept.
fn bad_value(flag: &str) -> &'static str {
    match flag {
        "timeout" | "concurrency" => "0",
        "url" => "ftp://example.com/generate_204",
        other => panic!("no bad value for `--{other}`"),
    }
}

/// Where the binary is, when this build produced one.
///
/// `target/debug/<bin>` beside the manifests, never a working directory: cargo
/// runs a test from the crate root, and a path built from `env!("CARGO_MANIFEST_DIR")`
/// is the same file wherever it is invoked from.
fn cvt_binary() -> Option<PathBuf> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.parent()?.parent()?;
    let target = std::env::var_os("CARGO_TARGET_DIR").map_or_else(
        || root.join("target"),
        |dir| PathBuf::from(dir).join("..").join("target"),
    );
    [
        target.join("debug").join("clash-verge-tui"),
        root.join("target").join("debug").join("clash-verge-tui"),
    ]
    .into_iter()
    .find(|candidate| candidate.is_file())
}

/// What one run of the binary did.
#[derive(Debug)]
struct Ran {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Ran {
    fn said(&self) -> String {
        format!("{}\n{}", self.stdout.trim(), self.stderr.trim())
    }
}

/// Run the binary against a home, with the given arguments.
fn run(home: &Path, args: &[&str]) -> Ran {
    let binary = cvt_binary().expect("the binary is built by `cargo test --workspace`");
    let out = Command::new(binary)
        .arg("--home")
        .arg(home)
        .args(args)
        .output()
        .expect("the binary runs");
    Ran {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// The `--help` page of one command path.
fn help(path: &str) -> String {
    let binary = cvt_binary().expect("the binary is built by `cargo test --workspace`");
    let mut command = Command::new(binary);
    for word in path.split_whitespace() {
        command.arg(word);
    }
    command.arg("--help");
    let out = command.output().expect("the binary runs");
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The subcommands one `--help` page lists.
fn subcommands(page: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut inside = false;
    for line in page.lines() {
        if line.trim_end() == "Commands:" {
            inside = true;
            continue;
        }
        if !inside {
            continue;
        }
        if line.trim().is_empty() {
            continue;
        }
        let Some(rest) = line.strip_prefix("  ") else {
            break;
        };
        let Some(name) = rest.split_whitespace().next() else {
            continue;
        };
        if name.starts_with('-') {
            break;
        }
        // A wrapped description line starts further in, past the name column.
        if rest.starts_with(|c: char| c.is_whitespace()) {
            continue;
        }
        out.push(name.to_owned());
    }
    out
}

/// Every leaf command path in the tree, walked from the binary's own help.
fn leaf_commands() -> Vec<String> {
    let mut leaves = Vec::new();
    let mut queue: Vec<String> = subcommands(&help("")).into_iter().collect();
    while let Some(path) = queue.pop() {
        let children = subcommands(&help(&path));
        if children.is_empty() {
            leaves.push(path);
        } else {
            for child in children {
                queue.push(format!("{path} {child}"));
            }
        }
    }
    leaves.sort();
    leaves
}

/// The long flags one `--help` page declares.
fn declared_flags(page: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in page.lines() {
        for token in line.split_whitespace() {
            if let Some(flag) = token.strip_prefix("--") {
                let name = flag.split(['=', '<']).next().unwrap_or(flag);
                if name != "help" && name != "version" {
                    out.insert(name.to_owned());
                }
            }
        }
    }
    out
}

/// A document declaring `groups` select groups, for the replay.
fn document_with_groups(endpoint: &str, groups: usize) -> String {
    let mut text = format!("mixed-port: 7890\nexternal-controller: {endpoint}\nmode: rule\n");
    text.push_str("proxies:\n");
    for name in ["node-a", "node-b"] {
        let _ = writeln!(
            text,
            "  - {{ name: {name}, type: socks5, server: 127.0.0.1, port: 1080 }}"
        );
    }
    text.push_str("proxy-groups:\n");
    for index in 0..groups {
        let _ = writeln!(
            text,
            "  - {{ name: grp-{index:02}, type: select, proxies: [node-a, node-b] }}"
        );
    }
    text.push_str("rules:\n  - MATCH,grp-00\n");
    text
}

/// An index remembering a choice for every group of [`document_with_groups`].
fn index_with_selections(groups: usize) -> String {
    let mut text = String::from(
        "current: L1\nitems:\n  - uid: L1\n    type: local\n    name: base\n    file: L1.yaml\n    selected:\n",
    );
    for index in 0..groups {
        let _ = writeln!(text, "      - name: grp-{index:02}\n        now: node-b");
    }
    text
}

/// A service whose remembered choices all point at a controller that never
/// answers.
fn service_against_black_hole(port: u16, groups: usize) -> (TempDir, Service) {
    let dir = TempDir::new().unwrap();
    let paths = AppPaths::new(dir.path());
    paths.ensure_dirs().unwrap();
    std::fs::write(paths.profiles_index(), index_with_selections(groups)).unwrap();
    std::fs::write(
        paths.profiles_dir().join("L1.yaml"),
        document_with_groups(&format!("127.0.0.1:{port}"), groups),
    )
    .unwrap();
    let service = Service::open(paths).unwrap();
    (dir, service)
}

/// Which of the four things `copy_state` writes are missing from a backup.
///
/// The class is "everything the home holds", walked from the home rather than
/// listed by hand: a backup that is missing an item is not a backup of this
/// home, whatever it does hold.
fn missing_items(home: &Path, backup: &Path) -> Vec<String> {
    let mut missing = Vec::new();
    for name in ["cvt.yaml", "profiles.yaml"] {
        if home.join(name).is_file() && !backup.join(name).is_file() {
            missing.push(name.to_owned());
        }
    }
    for name in ["profiles", "overrides"] {
        let Ok(entries) = std::fs::read_dir(home.join(name)) else {
            continue;
        };
        for entry in entries.flatten() {
            let child = format!("{name}/{}", entry.file_name().to_string_lossy());
            if entry.file_type().is_ok_and(|kind| kind.is_file())
                && !backup.join(name).join(entry.file_name()).is_file()
            {
                missing.push(child);
            }
        }
    }
    missing
}

// ==============================================================================
// 1. The settings screen: what a row shows, and what it writes
// ==============================================================================

/// CLAIM (`set_setting_text`'s doc): "Validating here rather than on save means
/// a mistyped URL is refused while the prompt is still open, with the reason
/// attached to it" — and the prompt's own contract, which is that it *edits the
/// value the row is showing*.
///
/// **Observed failing at `309ae4d`**, and fixed while this round ran (the
/// `editable` field, in the working tree at the time of writing), so this is
/// kept as the regression guard rather than as an open finding.
///
/// The seed *was* `row.value` (`toggle_setting` passed `row.value.clone()` into
/// `open_prompt`), and for three of the five text rows that string is not the
/// value: `core.binary` renders `discovered` when it is unset,
/// `core.external_controller` renders `from the base profile`, and
/// `core.secret` renders `set` or `none`. So pressing Enter to open the prompt
/// and Enter again to accept it — the two keystrokes a person makes to *look*
/// at a setting — wrote the rendering into the setting. Observed: 4 of 10
/// (row, state) pairs overwritten.
///
/// Asserted the way a user would notice it: after opening a text row's prompt
/// and committing it untouched, the setting must be what it was.
#[test]
fn confirmed_1_a_text_row_s_prompt_edits_the_value_not_the_rendering() {
    let dir = TempDir::new().unwrap();

    // Both states of the three rows whose rendering is not their value: the
    // defaults, where they are unset, and a home that has them set. A row is
    // only faithful to its value in one of the two.
    let mut unset = Settings::default();
    unset.core.binary = None;
    unset.core.external_controller = None;
    unset.core.secret = None;
    let mut set = unset.clone();
    set.core.binary = Some(PathBuf::from("/opt/mihomo/mihomo"));
    set.core.external_controller = Some("127.0.0.1:9090".to_owned());
    set.core.secret = Some("hunter2".to_owned());

    // The class, and the table, must be the same set.
    let class = text_keys(&unset);
    let table: BTreeSet<&str> = [
        "core.binary",
        "core.external_controller",
        "core.secret",
        "test.url",
        "test.expected_status",
    ]
    .into_iter()
    .collect();
    assert_eq!(
        class, table,
        "the text rows moved; extend this test's table rather than letting a \
         member of the class go unchecked"
    );

    let mut wrong = Vec::new();
    let mut tried = 0;
    for (state, settings) in [("unset", &unset), ("set", &set)] {
        for key in &class {
            tried += 1;
            let mut app = app_with(settings.clone(), dir.path());
            app.settings_rows.select_by_key(*key, |row| row.key);
            assert_eq!(
                app.settings_rows.selected_item().map(|row| row.key),
                Some(*key),
                "the cursor is not on `{key}`"
            );
            let before = text_setting(&app.settings, key);
            press(&mut app, KeyCode::Enter);
            let seed = seeded(&app);
            press(&mut app, KeyCode::Enter);
            let after = text_setting(&app.settings, key);
            if after != before {
                wrong.push(format!(
                    "`{key}` ({state}): the row shows `{seed}`, the prompt was seeded \
                     with it, and Enter-Enter wrote it — the setting is now `{after}`, \
                     and was `{before}`"
                ));
            }
        }
    }

    assert!(
        wrong.is_empty(),
        "a prompt seeded with what the row *displays* rather than what the setting \
         *is* turns the two keystrokes that open and close it into a silent edit. \
         {} of {tried} (row, state) pairs were overwritten with their own \
         rendering:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}

/// CLAIM (`Settings::core.external_controller`'s doc): "When this is set it
/// wins over every profile" — so what the settings screen puts there is forced
/// into every generated document, and must be something the generator accepts.
///
/// **Observed failing at `309ae4d`**: the value the two keystrokes produced was
/// this program's own rendering of "unset" (`from the base profile`), nothing
/// in `Settings::validate` looked at `core.external_controller`, and the
/// generator then refused every configuration with `E-CONTROLLER-FORMAT` while
/// the row went on displaying the same words for `None` and for the bad value.
/// Fixed while this round ran (the `editable` field), so this is kept as the
/// regression guard. The reconstruction beside it is what makes it a guard: the
/// rendering is still refused by the generator, which is *why* the prompt must
/// not carry it.
#[test]
fn confirmed_2_the_controller_a_text_row_writes_is_one_the_generator_accepts() {
    let dir = TempDir::new().unwrap();
    let mut app = app_with(Settings::default(), dir.path());
    app.settings_rows
        .select_by_key("core.external_controller", |row| row.key);
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Enter);

    // Exactly what the screen now holds, handed to the generator the way the
    // binary hands it: the settings' own control plane, over every profile.
    let held = app.settings.core.external_controller.clone();
    let settings_ok = app.settings.validate();

    let (_home, paths) = inhabited_home();
    let store = ProfileStore::load(&paths).unwrap();
    let outcome = Pipeline::new(paths.clone())
        .with_control_plane(held.as_deref(), None)
        .generate(&store)
        .unwrap();
    let codes: Vec<&str> = outcome
        .report
        .errors_iter()
        .map(|finding| finding.code)
        .collect();

    assert!(
        codes.is_empty(),
        "the settings screen holds `core.external_controller = {held:?}`, which \
         `Settings::validate` accepts ({settings_ok:?}) and which the generator \
         forces into every document: it refuses with {codes:?}"
    );
    assert!(
        held.is_none(),
        "committing the prompt untouched must not set it"
    );

    // The reconstruction: the string the row *displays* for that state is not a
    // control plane, which is why seeding the prompt with it was the defect.
    let displayed = cvt_tui::app::setting_rows(&Settings::default())
        .into_iter()
        .find(|row| row.key == "core.external_controller")
        .map(|row| row.value)
        .unwrap();
    let refused = Pipeline::new(paths)
        .with_control_plane(Some(&displayed), None)
        .generate(&store)
        .unwrap();
    let refused_codes: Vec<&str> = refused
        .report
        .errors_iter()
        .map(|finding| finding.code)
        .collect();
    assert!(
        !refused_codes.is_empty(),
        "`{displayed}` — what the row shows when the setting is unset — was \
         accepted as a control plane by the generator ({refused_codes:?}); if that \
         is true, seeding the prompt with the rendering was harmless, and the two \
         findings in this test are one"
    );
}

/// CLAIM (`set_settings`'s only caller, `Data::Settings`): the settings screen
/// shows what is on disk plus whatever the user has changed, and
/// `settings_dirty` is the flag that says there is a difference.
///
/// **Observed failing at `309ae4d`.** `set_settings` replaced the settings and
/// cleared the flag unconditionally, and the loop *asks* for that replacement
/// on its own: `on_tick` re-reads the current screen every `POLL_EVERY` ticks
/// while the core is running, and the settings screen's read is
/// `Effect::Refresh(Settings)`. So an edit that had not been saved was undone by
/// the clock, without a word, and the `s` that followed saved the *old* values
/// and reported success. Fixed while this round ran (a dirty guard in
/// `set_settings`), so this is kept as the regression guard.
///
/// The guard is what [`confirmed_11`] then found the other half of: the flag it
/// depends on has to be cleared by a *finished save*, and the binary does not
/// report one the interface listens for.
#[test]
fn confirmed_3_a_refresh_keeps_an_unsaved_edit() {
    let dir = TempDir::new().unwrap();
    let mut app = app_with(Settings::default(), dir.path());
    app.on_event(Event::Data(Data::Core(CoreStatus::Running {
        pid: std::process::id(),
        since: 0,
    })));
    app.settings_rows
        .select_by_key("core.auto_start", |row| row.key);
    press(&mut app, KeyCode::Char(' '));
    let edited = app.settings.core.auto_start;
    let dirty = app.settings_dirty;
    assert!(edited, "the space bar flipped the switch");
    assert!(dirty, "an edit is an edit");

    // The clock: the core is running, so the loop re-reads the screen it is on.
    let mut asked = false;
    for _ in 0..POLL_EVERY {
        for effect in app.on_tick() {
            if matches!(effect, Effect::Refresh(Screen::Settings)) {
                asked = true;
            }
        }
    }

    // What the binary answers: the file, which nobody has saved.
    app.on_event(Event::Data(Data::Settings(Box::default())));
    let after_refresh = (app.settings.core.auto_start, app.settings_dirty);
    let saved = press(&mut app, KeyCode::Char('s'));
    let written = saved.iter().find_map(|effect| match effect {
        Effect::SaveSettings { settings } => Some(settings.core.auto_start),
        _ => None,
    });

    assert!(
        asked && after_refresh.0 && written == Some(true),
        "the loop asked for the settings to be re-read ({asked}), and `set_settings` \
         put the disk's copy back over the user's: `core.auto_start` went \
         {edited} -> {} and `settings_dirty` {dirty} -> {}. `s` then saved \
         `{written:?}`, so the interface reported a save and wrote nothing. \
         Nothing on the settings screen tells the user the edit is gone.",
        after_refresh.0,
        after_refresh.1
    );
}

/// CONFIRMED: every edit the settings screen offers, applied until it wraps,
/// leaves settings the loader accepts — and the file they make survives a round
/// trip.
///
/// The ninth review's open question was whether a value the *flag* accepts can
/// be one the settings refuse; this asks the same of the interface, over every
/// row the screen offers rather than the ones the steppers were written for.
///
/// **A latent hole was found here and closed while the round ran** (commit
/// `b19cd97`): the three keys `set_setting_text` also accepts as free text —
/// `ui.refresh_ms`, `test.timeout_ms`, `test.concurrency` — took any parseable
/// number there while the settings refuse `0`, a timeout above the core's
/// `int16` and a concurrency above the file-descriptor ceiling. The branch was
/// *not reachable from the screen* (those three rows are `SettingKind::Number`,
/// `toggle_setting` opens a prompt only for `SettingKind::Text`, and
/// `commit_text_setting` commits against the selected row), so this test could
/// not reach it either; it is recorded here because "unreachable today" is not
/// the same as "cannot be wrong" — one new `Text` row would have made it live.
/// `set_setting_text` now writes, validates and rolls back, so every arm is
/// held to what a save would refuse.
#[test]
fn confirmed_4_every_edit_the_settings_screen_offers_survives_validation() {
    let (dir, paths) = inhabited_home();
    let mut app = app_with(Settings::default(), dir.path());
    let keys: Vec<&'static str> = cvt_tui::app::setting_rows(&app.settings)
        .into_iter()
        .map(|row| row.key)
        .collect();

    let mut refused = Vec::new();
    for key in &keys {
        app.settings = Settings::default();
        app.settings_rows =
            cvt_tui::state::Table::from_items(cvt_tui::app::setting_rows(&app.settings));
        app.settings_rows.select_by_key(*key, |row| row.key);
        let text_row = app
            .settings_rows
            .selected_item()
            .is_some_and(|row| row.kind == SettingKind::Text);
        // Every operation the screen offers for this row, applied until it has
        // been round the whole cycle (or, for text, opened and closed).
        for _ in 0..9 {
            if text_row {
                press(&mut app, KeyCode::Enter);
                press(&mut app, KeyCode::Esc);
            } else {
                press(&mut app, KeyCode::Char(' '));
            }
            if let Err(error) = app.settings.validate() {
                refused.push(format!(
                    "`{key}` ({}): {error}",
                    if text_row { "text" } else { "stepped" }
                ));
                break;
            }
        }
    }
    assert!(
        refused.is_empty(),
        "the screen offered an edit the loader refuses; saving it would leave a \
         `cvt.yaml` this program cannot start from:\n{}",
        refused.join("\n")
    );

    // And the file it makes is the file it reads back.
    app.settings = Settings::default();
    app.settings_rows.select_by_key("logs.keep", |row| row.key);
    press(&mut app, KeyCode::Char(' '));
    app.settings.save(&paths).unwrap();
    let reloaded = Settings::load(&paths).unwrap();
    assert_eq!(
        reloaded, app.settings,
        "a settings file this program wrote did not load back as itself"
    );
}

/// CLAIM (`App::settings_dirty`'s doc): "Whether the settings differ from what
/// is on disk."
///
/// **Observed failing at the start of this round's second half**, and fixed
/// while the round ran (the executor now reports `Done::SettingsSaved`), so
/// this is kept as the regression guard.
///
/// A finished save is what makes them agree again, and the interface has a
/// completion for it: `Done::SettingsSaved` clears the flag. Nothing in the
/// *binary* produced that variant — `Effect::SaveSettings` reported
/// `Data::Notice("settings saved")`, which the app handles as a status line —
/// so once `set_settings` stopped a refresh from overwriting unsaved edits, the
/// flag could never be cleared in a real session. What that cost: the settings
/// screen kept its own copy for the rest of the run (so an edit made in
/// another terminal was never seen again), and every refresh set the warning
/// "the settings on disk changed; your edits are kept" whether or not anything
/// changed.
///
/// The claim the test asserts is the flag's own doc, and the completion it feeds
/// the interface is read out of the executor rather than typed here — so it
/// points at the seam whichever side of it moves.
#[test]
fn confirmed_11_the_interface_waits_for_a_save_completion_the_binary_sends() {
    let dir = TempDir::new().unwrap();
    let mut app = app_with(Settings::default(), dir.path());
    app.settings_rows
        .select_by_key("core.auto_start", |row| row.key);
    press(&mut app, KeyCode::Char(' '));
    assert!(app.settings_dirty, "an edit is an edit");

    let asked = press(&mut app, KeyCode::Char('s'));
    assert!(
        matches!(asked.as_slice(), [Effect::SaveSettings { .. }]),
        "`s` asks for a save: {asked:?}"
    );

    // The save succeeded (nothing here can make it fail), so the settings no
    // longer differ from what is on disk.
    let completion = what_a_finished_save_reports();
    let _ = app.on_event(completion);
    assert!(
        !app.settings_dirty,
        "the save wrote the file and the interface still reports the settings as \
         differing from disk. `Done::SettingsSaved` is the variant that clears it \
         and nothing produces it: `Effect::SaveSettings` reports \
         `Data::Notice(\"settings saved\")` (`crates/cvt/src/executor.rs`), which \
         `on_data` turns into a status line. The guard that keeps a refresh from \
         discarding unsaved edits reads this flag, so it never stops guarding."
    );
}

/// What the binary reports when `service.save_settings()` returns.
///
/// Extracted from the executor rather than written here, for the same reason
/// this project's other cross-crate checks are textual: the variant the
/// interface waits for and the event the binary sends live in different crates,
/// and nothing else makes them agree.
fn what_a_finished_save_reports() -> Event {
    let source = include_str!("../../cvt/src/executor.rs");
    let at = source
        .find("Effect::SaveSettings")
        .expect("the executor has an arm for a save; point this test at it");
    let mut arm = &source[at..];
    // Up to the next arm, so a `Data::Notice` from somewhere else cannot be
    // mistaken for this one.
    for boundary in ["\n            other =>", "\n            Effect::"] {
        if let Some(end) = arm.find(boundary) {
            arm = &arm[..end];
        }
    }
    if arm.contains("Done::SettingsSaved") {
        return Event::Done(Done::SettingsSaved);
    }
    if let Some(found) = arm.find("Data::Notice(\"") {
        let start = found + "Data::Notice(\"".len();
        let stop = arm[start..]
            .find('"')
            .expect("the notice is a closed literal")
            + start;
        return Event::Data(Data::Notice(arm[start..stop].to_owned()));
    }
    panic!("the arm that runs a save reports neither a `Done` nor a notice: {arm}");
}

/// CLAIM (`settings`'s module doc): "Settings are validated as a whole
/// ([`Settings::validate`]) rather than field by field, because the interesting
/// mistakes are *relationships*: a concurrency of zero, a test timeout that
/// exceeds the core's `int16` limit."
///
/// **Observed failing in the middle of this round**, and fixed while it ran, so
/// this is kept as the regression guard.
///
/// `core.external_controller` is a relationship of exactly that kind and was not
/// in the list. It is forced over every profile — "When this is set it wins over
/// every profile, and profiles are not allowed to declare a control plane at
/// all" — so it is written into every generated document, and a value the
/// settings accepted and the generator refused was a file this program wrote and
/// then could not use.
///
/// The fix is checked against the generator's own rule rather than trusted: both
/// now refuse a controller without a colon, and nine values either side of that
/// boundary — empty, whitespace, `x`, `host`, `:9090`, `127.0.0.1:`, `a:b:c`,
/// `127.0.0.1:9090`, `https://x:1` — load and generate with the same verdict on
/// both sides.
///
/// The ninth review asked whether a value the *flag* accepts can be one the
/// settings refuse. This is the same question one crate over: a value the
/// settings accept can be one the generator refuses.
#[test]
fn confirmed_12_the_settings_refuse_a_control_plane_the_generator_refuses() {
    let (_dir, paths) = inhabited_home();
    std::fs::write(
        paths.settings_file(),
        "core:\n  external_controller: from the base profile\n",
    )
    .unwrap();

    let loaded = Settings::load(&paths).ok();
    let accepted = loaded.is_some();
    let saved = loaded
        .as_ref()
        .map(|settings| format!("{:?}", settings.save(&paths)));
    let held = loaded
        .as_ref()
        .and_then(|settings| settings.core.external_controller.clone())
        .unwrap_or_default();

    let store = ProfileStore::load(&paths).unwrap();
    let outcome = Pipeline::new(paths)
        .with_control_plane(Some(&held), None)
        .generate(&store)
        .unwrap();
    let codes: Vec<&str> = outcome
        .report
        .errors_iter()
        .map(|finding| finding.code)
        .collect();

    assert!(
        codes.is_empty(),
        "`Settings::load` accepted `core.external_controller: {held}` \
         ({accepted}), `save` wrote it back ({saved:?}), and every generated \
         configuration is then refused with {codes:?}. Every other relationship \
         the settings carry is checked as a whole; this one is forced over every \
         profile and is not checked at all."
    );
}

// ==============================================================================
// 2. The CLI flag class, enumerated from the binary
// ==============================================================================

/// CLAIM (`resolve_limits`' doc): "The one place those two flags are read, so a
/// command cannot take them and skip the checks."
///
/// Enumerated mechanically rather than read: the subcommand tree is walked from
/// the binary's `--help`, every leaf's declared flags are taken from its own
/// page, and every command that declares one of the three flags is run with a
/// value nothing should accept. A command that joins the class without the
/// checks fails this test twice over — once for the value it accepts, and once
/// for not being in the table.
#[test]
fn confirmed_5_every_command_declaring_a_latency_flag_holds_it_to_the_checks() {
    let Some(_) = cvt_binary() else {
        eprintln!("SKIP: no built binary; nothing to enumerate");
        return;
    };
    let (dir, paths) = inhabited_home();

    // The class, from the binary.
    let mut declaring: Vec<(String, String)> = Vec::new();
    for command in leaf_commands() {
        for flag in declared_flags(&help(&command)) {
            if LATENCY_FLAGS.contains(&flag.as_str()) {
                declaring.push((command.clone(), flag));
            }
        }
    }
    assert!(
        !declaring.is_empty(),
        "no command declares {LATENCY_FLAGS:?}; the enumeration itself is broken"
    );

    // Every member must be in the table, and every table entry must be a
    // member: a hand-written list that has drifted is a list that guards
    // nothing.
    let table: BTreeSet<(String, String)> = LATENCY_COMMANDS
        .iter()
        .flat_map(|(command, _)| {
            LATENCY_FLAGS
                .iter()
                .filter(|flag| declared_flags(&help(command)).contains(**flag))
                .map(|flag| ((*command).to_owned(), (*flag).to_owned()))
        })
        .collect();
    let members: BTreeSet<(String, String)> = declaring.iter().cloned().collect();
    assert_eq!(
        members, table,
        "the class of commands taking {LATENCY_FLAGS:?} and the table this test \
         checks are not the same set; a new member is a command nobody has run"
    );

    // And every member refuses a value nothing should accept.
    let mut accepted = Vec::new();
    for (command, flag) in &members {
        let (name, arguments) = LATENCY_COMMANDS
            .iter()
            .find(|(name, _)| name == command)
            .copied()
            .unwrap_or_else(|| panic!("`{command}` has no invocation in the table"));
        let value = bad_value(flag);
        let flag_arg = format!("--{flag}");
        // The command words, then what it needs, then the flag under test.
        let mut args: Vec<&str> = name.split_whitespace().collect();
        args.extend_from_slice(arguments);
        args.push(&flag_arg);
        args.push(value);
        // Nothing may need a network: the refusal has to come first.
        std::fs::remove_file(paths.runtime_config()).ok();
        std::fs::write(
            paths.settings_file(),
            "test:\n  timeout_ms: 5000\n  concurrency: 16\n",
        )
        .unwrap();
        let ran = run(dir.path(), &args);
        // The refusal names the flag, whichever command it came from: a command
        // that accepted the value would be on its way to the controller with a
        // message about something else.
        let mentioned = ran.said().contains(&format!("invalid value for {flag}"));
        if ran.code != 1 || !mentioned {
            accepted.push(format!(
                "`{name} --{flag} {value}` exited {} and said {:?}",
                ran.code,
                ran.said().trim()
            ));
        }
    }
    assert!(
        accepted.is_empty(),
        "the flags are held to the ceilings in one place so that a command cannot \
         take them and skip the checks:\n{}",
        accepted.join("\n")
    );
}

/// CLAIM (`MAX_TEST_CONCURRENCY`'s doc): "Shared with the command line rather
/// than written twice. The flag and the setting are the same number in two
/// places, and a ceiling that only guards the one in the settings file is one
/// somebody can walk around by typing it."
///
/// So the two places carry the same number, but they answer differently at the
/// edge of it, and the difference is deliberate and written down in
/// `resolve_limits`: a timeout beyond the core's `int16` is *refused* by both,
/// while a concurrency beyond the file-descriptor ceiling is refused by the
/// settings and *clamped* by the flag (and the clamp is reported). This test
/// records both answers side by side, so the next round has the pair settled
/// rather than a plausible reading of the doc comment.
#[test]
fn confirmed_6_the_flag_and_the_settings_answer_at_their_shared_ceiling() {
    let Some(_) = cvt_binary() else {
        eprintln!("SKIP: no built binary; nothing to compare");
        return;
    };
    let (dir, _paths) = inhabited_home();

    // `test.timeout_ms`, through the one command that reads the settings.
    let mut disagreements = Vec::new();
    for (value, expected) in [("0", false), ("1", true), ("32767", true), ("32768", false)] {
        let settings_ok = {
            let mut candidate = Settings::default();
            candidate.test.timeout_ms = value.parse().unwrap();
            candidate.validate().is_ok()
        };
        std::fs::write(
            dir.path().join("cvt.yaml"),
            format!("test:\n  timeout_ms: {value}\n"),
        )
        .unwrap();
        let home_ran = run(dir.path(), &["status"]);
        let loaded = home_ran.code == 0;
        if loaded != settings_ok || loaded != expected {
            disagreements.push(format!(
                "test.timeout_ms {value}: validate() says {settings_ok}, loading a \
                 file with it says {loaded}, and both should say {expected}"
            ));
        }
    }

    // The flag, for the same values, and the one place the two answers differ.
    // The settings file the loop above left behind is a bad one, and a bad one
    // fails before any flag is read — which would make every value below look
    // refused.
    let mut flag_timeout = Vec::new();
    for value in ["0", "1", "32767", "32768"] {
        std::fs::write(
            dir.path().join("cvt.yaml"),
            "test:\n  timeout_ms: 5000\n  concurrency: 16\n",
        )
        .unwrap();
        let ran = run(
            dir.path(),
            &["proxies", "test", "PROXY", "--timeout", value],
        );
        // With the value refused, the run stops before it needs a controller;
        // with it accepted, the next thing that happens is the missing
        // endpoint, which is a different message.
        let refused = ran.code == 1 && ran.said().contains("invalid value for timeout");
        flag_timeout.push((value, refused));
    }
    let refused: Vec<&str> = flag_timeout
        .iter()
        .filter(|(_, refused)| *refused)
        .map(|(value, _)| *value)
        .collect();
    assert_eq!(
        refused,
        vec!["0", "32768"],
        "`--timeout` and `test.timeout_ms` must refuse the same values; the run \
         refused {refused:?}"
    );

    let mut concurrency = Vec::new();
    for value in ["0", "1", "512", "513"] {
        let settings_ok = {
            let mut candidate = Settings::default();
            candidate.test.concurrency = value.parse().unwrap();
            candidate.validate().is_ok()
        };
        let ran = run(dir.path(), &["proxies", "test-all", "--concurrency", value]);
        concurrency.push((
            value.to_owned(),
            settings_ok,
            ran.code == 1 && ran.said().contains("invalid value for concurrency"),
        ));
    }
    // Recorded, and it now records agreement: 513 *was* the documented
    // divergence — the flag clamped where the settings refused — and the
    // sentence in `resolve_limits` that justified it was falsified by asking
    // the same question of the settings file, which is hand-written. Capping
    // what somebody wrote is worse than refusing it, so the flag refuses too.
    // "The settings refuse it" against "the flag refuses it" — the two
    // booleans have to be read in the same sense. Comparing `settings_ok` with
    // `flag_refused` directly asks whether *acceptance* differs from
    // *refusal*, which is true of every value and so says nothing; the first
    // version reported all four as divergent the moment the pair started
    // agreeing.
    let diverges: Vec<&str> = concurrency
        .iter()
        .filter(|(_, settings_ok, flag_refused)| *settings_ok == *flag_refused)
        .map(|(value, _, _)| value.as_str())
        .collect();
    let answers: Vec<(&str, bool, bool)> = concurrency
        .iter()
        .map(|(value, settings_ok, flag_refused)| (value.as_str(), *settings_ok, *flag_refused))
        .collect();
    assert_eq!(
        answers,
        vec![
            ("0", false, true),
            ("1", true, false),
            ("512", true, false),
            ("513", false, true),
        ],
        "`--concurrency` and `test.concurrency` answer the same at the ceiling \
         they share, for every value around it"
    );
    assert!(
        diverges.is_empty(),
        "the flag and the setting are one ceiling in two places: {diverges:?}"
    );
    assert!(
        disagreements.is_empty(),
        "the settings answer differently through the loader and through \
         `validate()`:\n{}",
        disagreements.join("\n")
    );
}

/// **Observed failing while this round ran**, and fixed in it (commit
/// `13adc95`, with the fetcher's own rule rather than a second one), so this is
/// kept as the regression guard.
///
/// CLAIM (`AddArgs.url`'s help): "Subscription URL to download from" — and
/// `add`'s own doc: "Add a subscription, download it, and leave it ready to
/// switch to."
///
/// The class is *every subscription address a person types*, and it has four
/// members: `--url` on the latency commands (`check_url_flag`: an http(s) URL or
/// a configured name, with the list in the message), `test.url` and
/// `test.urls[i].url` in the settings (both held to the prefix check), and the
/// two positional addresses — `profiles add` and `profiles edit-url`. The
/// positionals are checked for emptiness and for nothing else, and the two
/// outcomes differ: `edit-url` puts the previous address back when the fetch
/// fails, while `add` has no previous address to restore and says so in as many
/// words ("the profile exists either way").
///
/// That reasoning held for a *download* failure and not for this one: the
/// fetcher refuses `not-a-url` outright — "is not a URL" — so the retry the
/// program suggested was a command that could never succeed, and the index kept
/// a profile whose only purpose is to be fetched from an address this program
/// would not fetch from.
#[test]
fn confirmed_13_a_subscription_address_the_fetcher_refuses_is_recorded_anyway() {
    let Some(_) = cvt_binary() else {
        eprintln!("SKIP: no built binary; nothing to run");
        return;
    };
    let (dir, paths) = inhabited_home();

    let added = run(dir.path(), &["profiles", "add", "not-a-url"]);
    let index = std::fs::read_to_string(paths.profiles_index()).unwrap_or_default();
    let refused = added.said().contains("is not a URL");

    assert!(
        refused,
        "the fetcher must have refused the address, or this test is measuring \
         something else: {}",
        added.said().trim()
    );
    assert!(
        !index.contains("not-a-url"),
        "`profiles add not-a-url` recorded the address and then refused it in its \
         own fetcher (`is not a URL`). The index holds:\n{index}\n\
         `--url` refuses exactly this value (\"is not an http(s) URL, and no test \
         target is called that\") and `test.urls[i].url` is held to the same check; \
         the two positional addresses are checked for emptiness only, so the guard \
         covers the members somebody named. The empty string is refused before the \
         add; an address that is not one is not — and the hint the failure prints \
         (`retry with profiles update`) names a command that can never succeed."
    );
}

/// CLAIM (`UrlChangeReport::url`'s doc): "The URL it now points at" — and the
/// hint the failure prints: "`RX` points at the new URL but could not be
/// downloaded from it; retry with `clash-verge-tui profiles update RX`".
///
/// `edit-url` puts the previous address back when the fetch fails — round 8's
/// fix, and it works: the index is restored. What was not updated is everything
/// the command *says*. The report was written for the world before the rollback,
/// where the new address really did stay, so `url` names an address the profile
/// no longer has, and the hint names a command that would fetch the old one.
///
/// **Observed failing twice while this round ran**, and fixed in it, so this is kept
/// as the regression guard. The first state reported `url: not-a-url` and the hint
/// "points at the new URL" while the index held the old one; the second kept `url`
/// right and put the address that was *tried* in `previous`, whose doc says it is
/// "what it pointed at before" — a consumer rendering `previous -> url` read the move
/// backwards. Both fields and the hint now agree with the state the rollback leaves.
///
/// The fourth member of the URL class, one layer down from the address check above: there
/// the address was wrong and the state followed it, here the state is right and
/// what the program reports is not. A `--json` consumer is told the profile
/// moved to the address it was moved back from.
#[test]
fn confirmed_14_the_url_change_report_names_an_address_the_command_put_back() {
    let Some(_) = cvt_binary() else {
        eprintln!("SKIP: no built binary; nothing to run");
        return;
    };
    let (dir, paths) = inhabited_home();
    std::fs::write(
        paths.profiles_index(),
        "current: L1\nitems:\n  - uid: RX\n    type: remote\n    name: sub\n    \
         url: https://example.com/sub\n    file: RX.yaml\n  - uid: L1\n    type: local\n    \
         name: base\n    file: L1.yaml\n",
    )
    .unwrap();
    // What it pointed at before the command — read from the index rather than
    // written twice, so the assertion is against the state and not against a
    // second copy of the fixture.
    let before = url_in(&paths);

    let ran = run(
        dir.path(),
        &["profiles", "edit-url", "RX", "not-a-url", "--json"],
    );
    let now = url_in(&paths);
    let reported: serde_json::Value = serde_json::from_str(ran.stdout.trim())
        .unwrap_or_else(|error| panic!("`--json` prints one object: {error}: {}", ran.stdout));

    assert!(
        ran.code != 0,
        "the address is one the fetcher refuses, so the command must fail: {}",
        ran.said().trim()
    );
    let mut wrong = Vec::new();
    if now == before {
        // The rollback happened, which is what makes the report wrong rather
        // than merely confusing.
        if reported["url"].as_str() != Some(now.as_str()) {
            wrong.push(format!(
                "`cvt.profiles.url.v1` says `url: {}`, and the field's doc says it is \
                 \"The URL it now points at\" — which is `{now}`",
                reported["url"]
            ));
        }
        if reported["previous"].as_str() != Some(before.as_str()) {
            wrong.push(format!(
                "`previous` is {}, and the field's doc says it is \"What it pointed at \
                 before, when it had a URL at all\" — which is `{before}`, the address \
                 it still points at. The address that was *tried* is now in this field \
                 and nowhere else: a consumer rendering `previous -> url` reads the \
                 move backwards",
                reported["previous"]
            ));
        }
        if ran.stderr.contains("points at the new URL") {
            wrong.push(format!(
                "the failure says {:?}, and the profile points at the old address",
                ran.stderr.trim()
            ));
        }
    } else {
        wrong.push(format!(
            "the index holds `{now}` and held `{before}` before the command; without \
             the rollback there is nothing to report"
        ));
    }

    assert!(
        wrong.is_empty(),
        "`edit-url` put the previous address back — the index says so — and what the \
         command reports does not agree with the state it left:\n{}",
        wrong.join("\n")
    );
}

/// The `url:` line of the current profile, as the index holds it.
fn url_in(paths: &AppPaths) -> String {
    std::fs::read_to_string(paths.profiles_index())
        .unwrap_or_default()
        .lines()
        .find_map(|line| line.trim().strip_prefix("url: ").map(str::to_owned))
        .unwrap_or_else(|| "(none)".to_owned())
}

// ==============================================================================
// 3. Every loop that calls the client, against a controller that never answers
// ==============================================================================

/// CLAIM (`restore_selections`' doc): "Best effort per group"; and
/// `select_within`'s: "a core that answers `GET /group/…` and never answers the
/// `PUT` costs the *client's* timeout — ten seconds by default — which
/// is not the replay's budget and cannot be enforced from here."
///
/// The client's default timeout is ten seconds, so every read and every write in
/// the replay is bounded by a deadline the *client* owns unless the call itself
/// is wrapped. A controller that accepts the connection and answers nothing is
/// the only shape that tells the two apart, and twenty remembered choices make
/// the difference unmissable: unbounded, the replay would spend a hundred
/// seconds; bounded, it spends `REPLAY_TOTAL` (two) and says so.
#[tokio::test]
async fn confirmed_7_the_replay_is_bounded_by_a_controller_that_never_answers() {
    let (port, accepted) = black_hole();
    let groups = 20;
    let (_dir, service) = service_against_black_hole(port, groups);

    let started = Instant::now();
    let applied = service.restore_selections().await;
    let elapsed = started.elapsed();

    assert!(
        accepted.load(Ordering::SeqCst) > 0,
        "the controller was never asked, so nothing was bounded: {accepted:?}"
    );
    assert_eq!(applied.unwrap(), 0, "nothing can have taken");
    assert!(
        elapsed < Duration::from_secs(4),
        "`restore_selections` promises a two-second budget (`REPLAY_TOTAL`) and \
         returned after {elapsed:?}, with the client's own ten-second timeout as \
         the only bound any of its calls could have had: `read_group`, \
         `select_within` and `confirm_selection` are the class this project has \
         fixed five times"
    );
}

/// OBSERVED: what `geo`'s `--timeout` actually bounds.
///
/// `docs/CLI.md` says "how long to wait for an answer, default 10000", and the
/// command tries three sources in order. Each source gets its own request with
/// the flag as the client's timeout, so the *command* can take three times the
/// flag — the "per-item budgets that multiply" shape, in the CLI rather than in
/// the loop. `unlock` documents the same number as "per service", which is what
/// it is, so the same reading of `geo`'s row is the one that is wrong.
///
/// Measured, not asserted from the docs: a proxy port pointing at a listener
/// that never answers makes every request expire at exactly the flag.
#[test]
fn observed_8_geo_spends_the_timeout_once_per_source() {
    let Some(_) = cvt_binary() else {
        eprintln!("SKIP: no built binary; nothing to measure");
        return;
    };
    let (dir, paths) = inhabited_home();
    let (port, accepted) = black_hole();
    // `geo` reads the proxy port out of the generated configuration.
    std::fs::write(paths.runtime_config(), format!("mixed-port: {port}\n")).unwrap();

    let timeout_ms = 700;
    let started = Instant::now();
    let ran = run(dir.path(), &["geo", "--timeout", &timeout_ms.to_string()]);
    let elapsed = started.elapsed();

    assert!(
        accepted.load(Ordering::SeqCst) >= 3,
        "each of the three sources must have been tried through the black hole; \
         it saw {} connection(s) and the run said {:?}",
        accepted.load(Ordering::SeqCst),
        ran.said().trim()
    );
    assert!(
        elapsed >= Duration::from_millis(3 * timeout_ms - 200),
        "three sources at one timeout each should be at least three timeouts; the \
         run took {elapsed:?} for --timeout {timeout_ms}"
    );
    assert!(
        elapsed < Duration::from_millis(3 * timeout_ms + 2000),
        "measured {elapsed:?} for --timeout {timeout_ms}"
    );
}

// ==============================================================================
// 4. The backups, taken at once and taken badly
// ==============================================================================

/// CLAIM (`backup()`'s doc): "Named by the second it was taken, and given a
/// suffix when that second is taken … *Reserved*, not merely chosen", and
/// `prune_backups_keeping`'s: "Only what is *older* than the backup just
/// taken."
///
/// Round 8 found 24 threads being handed one directory and pruning each other
/// away, fixed it by reserving the name with `create_dir` and pruning only
/// strictly-older entries — and the fix was never run against the race it was
/// written for. This is that run, harder: four rounds of the same burst, and
/// the second half of the invariant the first version left out — a returned
/// path must *hold* the backup, every item the home has, not merely exist.
#[test]
fn confirmed_9_backups_taken_at_once_are_each_their_own_and_complete() {
    let (_dir, paths) = inhabited_home();
    let home = paths.home().to_path_buf();
    let service = Service::open(paths).unwrap();

    let threads: usize = 24;
    let rounds: usize = 4;
    let mut problems: Vec<String> = Vec::new();

    for round in 0..rounds {
        for backup in service.backups().unwrap() {
            let _ = std::fs::remove_dir_all(&backup.path);
        }
        let barrier = Arc::new(std::sync::Barrier::new(threads));
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
                Err(error) => problems.push(format!("round {round}: backup() failed: {error}")),
            }
        }

        let unique: BTreeSet<&PathBuf> = returned.iter().collect();
        if unique.len() != returned.len() {
            problems.push(format!(
                "round {round}: {} of {} threads were handed a directory another \
                 thread was handed as well",
                returned.len() - unique.len(),
                returned.len()
            ));
        }
        for path in &returned {
            let missing = missing_items(&home, path);
            if !missing.is_empty() {
                problems.push(format!(
                    "round {round}: {} was returned and is missing {missing:?}",
                    path.display()
                ));
            }
        }
        // Everything a thread was handed must still be listed: a prune is not
        // allowed to delete the backup that just returned it.
        let listed: BTreeSet<PathBuf> = service
            .backups()
            .unwrap()
            .into_iter()
            .map(|backup| backup.path)
            .collect();
        for path in &returned {
            if !listed.contains(path) {
                problems.push(format!(
                    "round {round}: {} was returned and is not among the backups \
                     any more",
                    path.display()
                ));
            }
        }
    }

    assert!(
        problems.is_empty(),
        "`backup()` reserves its name with `create_dir` and prunes only entries \
         older than its own second, so {threads} threads in {rounds} rounds must \
         each be handed their own complete directory. {} call(s) went wrong:\n{}",
        problems.len(),
        problems.join("\n")
    );
}

/// CLAIM (`backups()`' doc): "Every backup, newest first" — a directory this
/// program *took*; and [`BACKUP_LIMIT`]'s: "Keep the newest `keep` backups".
///
/// **Observed failing at `309ae4d`.** `backup()` reserved the directory first
/// and filled it a moment later, and the reservation was not undone when the
/// copy failed, so a backup that failed halfway left a directory that
/// `backups()` listed, that `looks_like_a_backup` admitted, and — because the
/// list is what the keep-limit counts — that evicted a real backup when the
/// next one was taken: the phantom was *newer* (higher sequence in the same
/// second), so the oldest real ones went first. Measured: five listed, four
/// holding the whole home, with the phantom among the survivors. Fixed while
/// this round ran (`backup()` removes the reservation when the copy fails), so
/// this is kept as the regression guard.
///
/// The minimal reproduction is the first half: a failed `backup()` must leave
/// the backups directory as it found it.
#[test]
fn confirmed_10_a_backup_that_failed_leaves_nothing_behind() {
    let (_dir, paths) = inhabited_home();
    let home = paths.home().to_path_buf();
    let service = Service::open(paths.clone()).unwrap();

    // Five real backups, all in the second the clock is in now.
    for _ in 0..BACKUP_LIMIT {
        service.backup().unwrap();
    }
    let real: BTreeSet<PathBuf> = service
        .backups()
        .unwrap()
        .into_iter()
        .map(|backup| backup.path)
        .collect();
    assert_eq!(real.len(), BACKUP_LIMIT, "five backups, five directories");

    // A copy that fails after the two scalar files are in place: the profile
    // documents are unreadable, which is a home mid-repair rather than a
    // contrived one.
    let documents = paths.profiles_dir();
    let mut permissions = std::fs::metadata(&documents).unwrap().permissions();
    let restore = permissions.clone();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o000);
    std::fs::set_permissions(&documents, permissions).unwrap();
    let failed = service.backup();
    std::fs::set_permissions(&documents, restore).unwrap();

    let after: BTreeSet<PathBuf> = service
        .backups()
        .unwrap()
        .into_iter()
        .map(|backup| backup.path)
        .collect();
    let leaked: Vec<&PathBuf> = after.difference(&real).collect();
    let incomplete: Vec<String> = after
        .iter()
        .map(|path| (path, missing_items(&home, path)))
        .filter(|(_, missing)| !missing.is_empty())
        .map(|(path, missing)| format!("{} is missing {missing:?}", path.display()))
        .collect();

    // The consequence: the next real backup prunes around a list the phantom
    // is part of, and the phantom is newer than every real one.
    let mut evicted = Vec::new();
    if failed.is_err() && !leaked.is_empty() {
        // One more second, so the new backup is strictly newer than everything
        // listed and the prune has something older than itself to remove.
        let before = newest_stamp(&real);
        let deadline = Instant::now() + Duration::from_secs(5);
        while newest_stamp(&real) == before && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        if let Ok(fresh) = service.backup() {
            let survivors: Vec<PathBuf> = service
                .backups()
                .unwrap()
                .into_iter()
                .map(|backup| backup.path)
                .collect();
            let complete: Vec<&PathBuf> = survivors
                .iter()
                .filter(|path| missing_items(&home, path).is_empty())
                .collect();
            if !survivors.contains(&fresh) || complete.len() < BACKUP_LIMIT {
                evicted.push(format!(
                    "after the next backup, {} director(y|ies) are listed and only {} \
                     hold the whole home: {:?}",
                    survivors.len(),
                    complete.len(),
                    survivors
                        .iter()
                        .map(|path| path.file_name().unwrap_or_default().to_string_lossy())
                        .collect::<Vec<_>>()
                ));
            }
        }
    }

    assert!(
        failed.is_err(),
        "the unreadable `profiles/` should have made this backup fail"
    );
    assert!(
        leaked.is_empty() && incomplete.is_empty() && evicted.is_empty(),
        "a backup that failed left {} director(y|ies) behind: {leaked:?}. \
         `backups()` lists them, `looks_like_a_backup` admits them — {incomplete:?} \
         — and the keep-limit counts them: {evicted:?}. The path `backup()` \
         returned nothing for is the one `restore` would offer as the newest \
         backup of a home it never finished copying.",
        leaked.len()
    );
}

/// The newest second among a set of backups, read from their names.
///
/// Used only to wait for the clock to leave the second the real backups were
/// taken in; the number itself never appears in an assertion.
fn newest_stamp(paths: &BTreeSet<PathBuf>) -> i64 {
    paths
        .iter()
        .filter_map(|path| path.file_name())
        .filter_map(|name| name.to_string_lossy().split('-').next().map(str::to_owned))
        .filter_map(|stamp| stamp.parse::<i64>().ok())
        .max()
        .unwrap_or(0)
}
