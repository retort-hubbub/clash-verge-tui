//! An **eleventh** independent pass, aimed at the four surfaces the tenth
//! review wrote down as not reached, and at the shape this project has now
//! produced nine times: *a guard that covers the members somebody named rather
//! than the class they belong to*.
//!
//! Method, in the order the review was asked to attack:
//!
//! * **`--node`/`--group` as a class**, enumerated **mechanically from the
//!   binary's own `--help`**: the leaf commands are walked from the help pages
//!   and the class is "every leaf whose page mentions `NODE` or `GROUP`". A
//!   seventh member joining the class fails
//!   [`confirmed_the_name_taking_class_is_exactly_these_seven_commands`] rather
//!   than going unchecked, which is the failure mode the tenth review's test
//!   was written to avoid. Every member is then run against a **real**
//!   `/usr/bin/verge-mihomo` with a name it does not have, and with a name it
//!   has, and with the name of a *node* where a *group* belongs;
//! * **the renderers** (`crates/cvt-tui/src/ui/**`): `render_tests.rs` claims
//!   every screen is drawn empty and populated at four sizes, and this file
//!   checks that claim rather than trusting it — then goes past it, at a
//!   terminal of zero columns, with a 4000-character name, with wide
//!   characters, and at the sizes the existing tests do not use;
//! * **`run.rs`**: the terminal scope, given a real terminal and taken back,
//!   with `raw mode` read out of the pty's own termios rather than inferred;
//! * **the global flags** (`--home`, `--json`, `-v`, `--no-color`), each held
//!   to what `docs/CLI.md` and `--help` say it does.
//!
//! `defect_*` names preserve the original findings; all tests now must pass.
//! Tests named `confirmed_*` assert a claim that was checked and holds; `observed_*`
//! record a measurement whose verdict is the author's to make.
//!
//! ### Why this file is in `cvt-tui`
//!
//! Half of what it attacks is the interface, and an integration test of
//! `cvt-tui` can use `cvt-core` (a dependency of this crate), the built binary
//! and the real core as well. `cargo test --workspace` runs all of them.
//!
//! ### The state each finding was observed in
//!
//! The tree moved twice while this was being written, which is what the round
//! was told to expect. Every test below was first run against **`f759335`**
//! (tag `v0.4.1`), where five of them failed; three of those five no longer
//! fail at **`d79a4f2`** plus the working tree as it stood at 09:0x, because
//! the author fixed them while the file was being written:
//!
//! | finding | `f759335` | now |
//! |---|---|---|
//! | `--no-color` does not reach the interface | fails | fixed in the working tree (`cvt/src/tui.rs`, `cvt/src/output.rs`, uncommitted) |
//! | a node where a group belongs is reported as an empty group | fails | fixed in the working tree (`commands/proxies.rs`, `commands/test.rs`, uncommitted) |
//! | the confirmation overlay loses its buttons when the question wraps | fails | fixed in the working tree (`ui/mod.rs::confirm`, uncommitted) |
//! | the prompt caret disappears when the value is wider than the popup | fails | fixed in the working tree (`ui/mod.rs::prompt`, uncommitted) |
//! | the wrapped log pane hides the newest lines | fails | fixed in the working tree (`ui/logs.rs`, uncommitted) |
//! | the empty `--name` | recorded once | **could not reproduce**; see below |
//!
//! The five fixes landed while this file was being written, and the tests for
//! the five findings now pass against them — they are the regression guards for
//! a fix that is not yet committed. Three of the fixes were then attacked as
//! code rather than trusted, and each has a defect of its own, all three
//! generated sweeps that fail against the working tree:
//!
//! | finding against a fix | what it is |
//! |---|---|
//! | [`defect_the_log_window_measures_a_shorter_row_than_it_draws`] | measures `at level message`, draws `at level<pad7> message`: three columns short, one row short at nine message lengths |
//! | [`defect_the_confirmation_popup_is_sized_in_characters_not_columns`] | `wrapped_rows` counts `chars()` where the terminal counts columns: 164 of 240 CJK names lose the buttons |
//! | [`defect_the_prompt_window_is_measured_in_characters_not_columns`] | the window is `chars` and the popup is columns: 37 of 192 caret positions, every one with wide characters |
//!
//! All three were fixed in turn (`3afc4d1` and the working tree), and the fixes
//! were then attacked with hostile input rather than trusted: eight log levels
//! (including one longer than the seven-column pad) over every message length,
//! seven kinds of profile name (spaces, CJK, emoji, ZWJ sequences, combining
//! marks) at six widths, and every caret position of four kinds of value. Two
//! of those sweeps are clean; the third found one more:
//!
//! | finding | state |
//! |---|---|
//! | [`defect_the_caret_is_not_drawn_when_it_sits_on_a_combining_mark`] | **still failing** — pre-existing, not a regression: the caret's cell holds a zero-width character and ratatui draws nothing, so no caret appears at 78 of 156 cursor positions |
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
    clippy::cast_possible_truncation,
    clippy::cast_lossless,
    clippy::redundant_clone,
    clippy::needless_pass_by_value,
    clippy::uninlined_format_args,
    clippy::module_name_repetitions,
    clippy::missing_panics_doc,
    clippy::doc_markdown,
    clippy::case_sensitive_file_extension_comparisons
)]

use std::collections::BTreeSet;
use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::Color;
use tempfile::TempDir;

use cvt_tui::app::{App, Data, Event, Overlay, Preview, PromptKind};
use cvt_tui::row::{ConnectionRow, LogRow, NodeRow, ProfileRow};
use cvt_tui::{Action, Screen, Theme};

// ==================================================================== helpers

/// The core this project is validated against. `-t` exits on its own.
const CORE: &str = "/usr/bin/verge-mihomo";

/// Where the binary is, when this build produced one.
///
/// `target/debug/<bin>` beside the manifests, never a working directory: cargo
/// runs a test from the crate root, and `CARGO_MANIFEST_DIR` is the same path
/// wherever it is invoked from.
fn cvt_binary() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = manifest
        .parent()
        .and_then(Path::parent)
        .expect("crates/cvt-tui has a workspace above it");
    let candidates = [
        root.join("target").join("debug").join("clash-verge-tui"),
        std::env::var_os("CARGO_TARGET_DIR").map_or_else(
            || root.join("target").join("debug").join("clash-verge-tui"),
            |dir| PathBuf::from(dir).join("debug").join("clash-verge-tui"),
        ),
    ];
    candidates
        .into_iter()
        .find(|candidate| candidate.is_file())
        .expect("`cargo test --workspace` builds the binary this file drives")
}

/// What one run of the binary did.
#[derive(Debug)]
struct Ran {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Ran {
    /// Everything it said, in one string, for a substring test.
    fn said(&self) -> String {
        format!("{}\n{}", self.stdout.trim(), self.stderr.trim())
    }
}

/// Run the binary against a home, with the given arguments and environment.
fn run_env(home: &Path, args: &[&str], envs: &[(&str, &str)]) -> Ran {
    let mut command = Command::new(cvt_binary());
    command.arg("--home").arg(home).args(args);
    for (key, value) in envs {
        command.env(key, value);
    }
    let out = command.output().expect("the binary runs");
    Ran {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn run(home: &Path, args: &[&str]) -> Ran {
    run_env(home, args, &[])
}

/// The `--help` page of one command path.
fn help(path: &str) -> String {
    let mut command = Command::new(cvt_binary());
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
        if !inside || line.trim().is_empty() {
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

/// The name of every option and positional one `--help` page declares.
///
/// A page names a positional as `<GROUP>`/`[GROUP]` and an option as
/// `--group <G>`; both spellings are what a user reads, so both are what the
/// class is derived from.
fn declared_names(page: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in page.lines() {
        for token in line.split_whitespace() {
            let token = token.trim_matches(['.', ',', ';', ')']);
            if let Some(flag) = token.strip_prefix("--") {
                let name = flag.split(['=', '<']).next().unwrap_or(flag);
                out.insert(name.to_ascii_uppercase());
            } else if let Some(inner) = token
                .strip_prefix('<')
                .or_else(|| token.strip_prefix('['))
                .and_then(|rest| rest.strip_suffix('>').or_else(|| rest.strip_suffix(']')))
            {
                out.insert(inner.to_ascii_uppercase());
            }
        }
    }
    out
}

/// `true` when a leaf's own help page offers somewhere to put a node or group.
fn takes_a_name(page: &str) -> bool {
    let names = declared_names(page);
    names.contains("NODE") || names.contains("GROUP")
}

/// Every leaf command that takes a node or a group name, **derived from the
/// binary's own `--help`** rather than listed here.
fn name_taking_commands() -> Vec<String> {
    leaf_commands()
        .into_iter()
        .filter(|path| takes_a_name(&help(path)))
        .collect()
}

/// The invocation of each name-taking command, with a positional supplied.
///
/// Hand-written because a positional has to be given a value, but *checked*
/// against the binary's own help: the class above is derived, and
/// [`confirmed_the_name_taking_class_is_exactly_these_seven_commands`] fails
/// when the two disagree, so a seventh member cannot join unchecked.
const NAME_CALLS: &[(&str, &[&str])] = &[
    ("proxies chain", &["proxies", "chain", "@"]),
    ("proxies list", &["proxies", "list", "@"]),
    ("proxies select", &["proxies", "select", "@", "@"]),
    ("proxies test", &["proxies", "test", "@"]),
    ("proxies unpin", &["proxies", "unpin", "@"]),
    ("test delay", &["test", "delay", "--group", "@"]),
    ("test urls", &["test", "urls", "--node", "@"]),
];

/// One call from [`NAME_CALLS`], with `@` replaced by a name.
fn with_name(template: &[&str], name: &str) -> Vec<String> {
    template
        .iter()
        .map(|word| (*word).replace('@', name))
        .collect()
}

// ---------------------------------------------------------------- live core

/// A real core, plus a home that points the binary at it.
///
/// Everything is inside one `TempDir`: the core's working directory, its
/// configuration and the CLI's application home.
struct Sandbox {
    _dir: TempDir,
    core: Child,
    home: PathBuf,
}

impl Sandbox {
    /// The document both the core and the binary are pointed at.
    fn document(port: u16) -> String {
        format!(
            "mixed-port: {proxy_port}\n\
             external-controller: 127.0.0.1:{port}\n\
             secret: \"\"\n\
             mode: rule\n\
             proxies:\n\
             \x20 - {{name: node-a, type: socks5, server: 127.0.0.1, port: 1080}}\n\
             \x20 - {{name: node-b, type: socks5, server: 127.0.0.1, port: 1081}}\n\
             proxy-groups:\n\
             \x20 - {{name: grp-select, type: select, proxies: [node-a, node-b]}}\n\
             \x20 - {{name: grp-url, type: url-test, url: \"http://127.0.0.1:9/\", interval: 600, proxies: [node-a, node-b]}}\n\
             rules:\n\
             \x20 - MATCH,grp-select\n",
            proxy_port = port + 1,
            port = port,
        )
    }

    /// Start a core, or `None` when this machine has none to start.
    ///
    /// The port is **retried**, and readiness is not `/version`: mihomo
    /// answers `/version` while `/proxies` is still short of its groups, which
    /// made `proxies list grp-select` fail with "no group or proxy named" on
    /// the first run and succeed on the second. A test that trusted
    /// `/version` would have recorded that as a defect.
    ///
    /// A machine with no core at all skips; a machine that has one and cannot
    /// bring it up **fails**, because every test that uses this would otherwise
    /// pass while checking nothing.
    fn start() -> Option<Self> {
        if !Path::new(CORE).is_file() {
            eprintln!("no core at {CORE}: this test checked nothing");
            return None;
        }
        let dir = TempDir::new().expect("a temporary directory");
        for attempt in 0..8 {
            let Some(port) = free_port() else { continue };
            let core_dir = dir.path().join(format!("core-{attempt}"));
            let home = dir.path().join(format!("home-{attempt}"));
            std::fs::create_dir_all(core_dir.join("data")).expect("the core's directory");
            std::fs::create_dir_all(home.join("profiles")).expect("the home directory");
            let mut settings = cvt_core::Settings::default();
            settings.core.external_controller = Some(format!("127.0.0.1:{port}"));
            settings.core.secret = Some(String::new());
            settings.save(&cvt_core::AppPaths::new(&home)).unwrap();
            let document = Self::document(port);
            std::fs::write(core_dir.join("config.yaml"), document.as_bytes())
                .expect("the core's configuration");
            std::fs::write(home.join("profiles").join("L1.yaml"), document.as_bytes())
                .expect("the profile document");
            std::fs::write(
                home.join("profiles.yaml"),
                "current: L1\nitems:\n  - uid: L1\n    type: local\n    name: base\n    file: L1.yaml\n",
            )
            .expect("the index");

            let mut core = Command::new(CORE)
                .arg("-d")
                .arg(core_dir.join("data"))
                .arg("-f")
                .arg(core_dir.join("config.yaml"))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("the core starts");
            if self::wait_for_proxies(port, &["node-a", "node-b", "grp-select", "grp-url"]) {
                return Some(Self {
                    _dir: dir,
                    core,
                    home,
                });
            }
            let _ = core.kill();
            let _ = core.wait();
        }
        panic!(
            "{CORE} is installed but eight ports in a row would not bring it up; \
             this test cannot check anything without it"
        );
    }

    /// Run the binary against this sandbox's home.
    fn run(&self, args: &[&str]) -> Ran {
        let owned: Vec<&str> = args.to_vec();
        run(&self.home, &owned)
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = self.core.kill();
        let _ = self.core.wait();
    }
}

/// A port nothing is listening on, for the core to bind.
fn free_port() -> Option<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").ok()?;
    let port = listener.local_addr().ok()?.port();
    drop(listener);
    Some(port)
}

/// `GET` over a raw socket, so the harness needs no HTTP client.
fn http_get(port: u16, path: &str) -> Option<String> {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_millis(500)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    write!(
        stream,
        "GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    let mut body = String::new();
    stream.read_to_string(&mut body).ok()?;
    Some(body)
}

/// Wait until the controller's proxy table holds every name that is expected.
fn wait_for_proxies(port: u16, expected: &[&str]) -> bool {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if let Some(body) = http_get(port, "/proxies")
            && expected.iter().all(|name| body.contains(name))
        {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

// ------------------------------------------------------------ the interface

/// Draw one frame and return the buffer's rows, one string per row.
fn draw(app: &App, width: u16, height: u16) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test backend");
    terminal
        .draw(|frame| cvt_tui::ui::render(frame, app))
        .expect("draw");
    buffer_rows(terminal.backend().buffer())
}

fn buffer_rows(buffer: &Buffer) -> Vec<String> {
    (0..buffer.area().height)
        .map(|y| {
            (0..buffer.area().width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect()
}

/// Collapse runs of whitespace so a phrase can be looked for across cells.
fn flat(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn app_with(theme: Theme) -> App {
    App::new(PathBuf::from("/tmp/cvt-terminal-rendering"), theme)
}

/// An application with nothing loaded, showing `screen`.
fn empty(screen: Screen) -> App {
    let mut app = app_with(Theme::default());
    app.screen = screen;
    app
}

/// An application with rows on every screen and a running core.
fn populated(screen: Screen) -> App {
    let mut app = app_with(Theme::default());
    for data in sample_data() {
        let _ = app.on_event(Event::Data(data));
    }
    app.screen = screen;
    app
}

fn sample_data() -> Vec<Data> {
    vec![
        Data::Core(cvt_core::mihomo::supervisor::CoreStatus::Running {
            pid: 4321,
            since: 0,
        }),
        Data::Version("v1.19.31".to_owned()),
        Data::Profiles(vec![ProfileRow::from_item(
            &cvt_core::profile::item::PrfItem::local("base", "base"),
            true,
            false,
        )]),
        Data::Nodes(vec![
            NodeRow {
                name: "PROXY".to_owned(),
                kind: "Selector".to_owned(),
                group: None,
                delay: Some(30),
                alive: true,
                active: false,
                is_group: true,
                is_proxy: false,
                members: 2,
                selectable: true,
            },
            NodeRow {
                name: "JP 01".to_owned(),
                kind: "Vless".to_owned(),
                group: Some("PROXY".to_owned()),
                delay: Some(20),
                alive: true,
                active: true,
                is_group: false,
                is_proxy: true,
                members: 0,
                selectable: true,
            },
        ]),
        Data::Connections(vec![ConnectionRow {
            id: "c1".to_owned(),
            destination: "example.com:443".to_owned(),
            network: "tcp".to_owned(),
            process: "curl".to_owned(),
            rule: "MATCH".to_owned(),
            chain: "PROXY → JP 01".to_owned(),
            upload: 1024,
            download: 65_536,
            started: "10:00:00".to_owned(),
        }]),
        Data::RuleProviders(vec!["geosite".to_owned()]),
        Data::Traffic(cvt_core::mihomo::types::Traffic {
            up: 2048,
            down: 1_048_576,
            up_total: 10_000,
            down_total: 9_000_000,
        }),
        Data::Memory(52_428_800),
        Data::Log(LogRow::new("info", "core is up")),
        Data::Preview(Box::new(Preview {
            summary: "1 proxy".to_owned(),
            changes: Vec::new(),
            findings: Vec::new(),
            warnings: Vec::new(),
            applicable: true,
            truncated: false,
        })),
    ]
}

// =================================== 1. `--node`/`--group` as a class

/// The class, enumerated from the binary rather than from this file.
///
/// The claim this guards is the one the tenth review's file was written for: a
/// guard that covers the members somebody named rather than the class they
/// belong to. A seventh command that takes a node or a group name fails **this**
/// test with "extend the table", which is the only way a new member can stay
/// unchecked.
#[test]
fn confirmed_the_name_taking_class_is_exactly_these_seven_commands() {
    let derived = name_taking_commands();
    let listed: Vec<String> = NAME_CALLS
        .iter()
        .map(|(path, _)| (*path).to_owned())
        .collect();
    assert_eq!(
        derived, listed,
        "the binary's own --help and `NAME_CALLS` disagree; a name-taking \
         command that is not in the table is one this file never checks"
    );
    // And the invocation table is really an invocation of the command it names.
    for (path, template) in NAME_CALLS {
        let mut command = Command::new(cvt_binary());
        for word in with_name(template, "NAME-PLACEHOLDER") {
            command.arg(word);
        }
        command.arg("--help");
        let out = command.output().expect("the binary runs");
        assert_eq!(
            out.status.code(),
            Some(0),
            "`{}` is not a command path: {}",
            path,
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// Every one of the seven, given a name the controller does not have.
///
/// This is the class check, and it **holds**: all seven refuse, all seven exit
/// non-zero, and all seven name the name they were given. The interesting
/// difference between the members is not *whether* they refuse but *what they
/// say*, which is where the two tests below look.
#[test]
fn confirmed_every_name_taking_command_refuses_a_name_the_controller_does_not_have() {
    let Some(sandbox) = Sandbox::start() else {
        eprintln!("no core at {CORE}; skipped");
        return;
    };
    let mut wrong: Vec<String> = Vec::new();
    for (path, template) in NAME_CALLS {
        let args = with_name(template, "no-such-name");
        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
        let ran = sandbox.run(&borrowed);
        if ran.code == 0 || !ran.said().contains("no-such-name") {
            wrong.push(format!("{path}: exit {} said {:?}", ran.code, ran.said()));
        }
    }
    assert!(
        wrong.is_empty(),
        "commands that accepted, or failed to name, a name the controller does \
         not have: {wrong:?}"
    );
}

/// The checks are checks, not blanket refusals.
///
/// Without this, a `proxies list` that refused everything would pass the test
/// above. Every name here is one the controller has, and every call must exit
/// 0. `proxies unpin grp-url` is preceded by a pin because clearing nothing is
/// not the case worth asserting.
#[test]
fn confirmed_the_name_checks_are_not_blanket_refusals() {
    let Some(sandbox) = Sandbox::start() else {
        eprintln!("no core at {CORE}; skipped");
        return;
    };
    let known: &[&[&str]] = &[
        &["proxies", "list", "grp-select"],
        &["proxies", "chain", "node-a"],
        &["proxies", "select", "grp-select", "node-a"],
        &["proxies", "select", "grp-url", "node-b"],
        &["proxies", "unpin", "grp-url"],
        &["proxies", "test", "grp-select"],
        &["test", "delay", "--group", "grp-select"],
        &["test", "urls", "--node", "node-a"],
    ];
    let mut wrong: Vec<String> = Vec::new();
    for args in known {
        let ran = sandbox.run(args);
        if ran.code != 0 {
            wrong.push(format!("{args:?}: exit {} said {:?}", ran.code, ran.said()));
        }
    }
    assert!(
        wrong.is_empty(),
        "commands that refused a name the controller has: {wrong:?}"
    );
}

/// CLAIM (`crates/cvt-tui`'s CLI, `proxies list`): a name that is **a node** is
/// refused with "`X` is a socks5 node, not a group".
///
/// The same claim has to hold for every command that takes a *group* name, and
/// that is the class this project has now got wrong nine times. It is checked
/// in exactly one of the three places a group name is accepted:
///
/// * `proxies list <node>` — `if !target.is_group()` in
///
///  `crates/cvt/src/commands/proxies.rs`, which says "`node-a` is a socks5
///
///  node, not a group";
/// * `proxies test <node>` — no `is_group` check, so the empty member list
///
///  becomes "`node-a` has no members to test";
/// * `test delay --group <node>` — no `is_group` check, so it becomes "nothing
///
///  to test: the selection has no nodes".
///
/// Both of the last two are **wrong about what the user did**: the thing has no
/// members because it is not a group, not because a group is empty, and the one
/// command that says so is the one somebody thought of. A user who types a node
/// where a group belongs is told to go looking for an empty group.
///
/// The names come from the controller rather than from a literal: `proxies
/// list node-a` exits 0 only because the controller has `node-a`, so the check
/// is "a name the controller has and which is not a group".
/// Observed failing at `f759335`; the working tree fixes it (`is_group` in `commands/proxies.rs`///
/// and `commands/test.rs`) and this is the guard for that fix.
#[test]
fn defect_a_name_that_is_a_node_is_not_reported_as_not_a_group() {
    let Some(sandbox) = Sandbox::start() else {
        eprintln!("no core at {CORE}; skipped");
        return;
    };
    let cases: &[&[&str]] = &[
        &["proxies", "list", "node-a"],
        &["proxies", "test", "node-a"],
        &["test", "delay", "--group", "node-a"],
    ];
    let mut wrong: Vec<String> = Vec::new();
    for args in cases {
        let ran = sandbox.run(args);
        let said = ran.said();
        // Every one of them must refuse; the claim is about the reason.
        assert_ne!(ran.code, 0, "{args:?} accepted a node as a group");
        if !said.contains("not a group") {
            wrong.push(format!("{args:?}: exit {}, said {said:?}", ran.code));
        }
    }
    assert!(
        wrong.is_empty(),
        "commands that refused a node where a group belongs without saying it \
         is not a group: {wrong:?}"
    );
}

/// RECORD: `proxies unpin` depends on the core for everything, including the
/// reason, and the core's reason for the most common group kind is "Body
/// invalid".
///
/// Measured against `/usr/bin/verge-mihomo v1.19.31`:
///
/// ```text
/// $ cvt proxies unpin grp-url      # URLTest, pinned first
/// `grp-url` is back on its own strategy                       [exit 0]
/// $ cvt proxies unpin grp-select   # Selector
/// error: core api DELETE /proxies/grp-select returned 400: Body invalid  [exit 1]
/// $ cvt proxies unpin node-a       # a plain proxy
/// error: core api DELETE /proxies/node-a returned 400: Body invalid      [exit 1]
/// ```
///
/// The outcome is **right** — a `Selector` has no pin to clear, so the core
/// refuses — and [`cvt_core::mihomo::client::Client::clear_selection`]
/// documents the 400. What is recorded here is that the message a user meets
/// names neither the group kind nor the reason, while its sibling
/// `proxies select` gets "Must be a Selector" from the same core, and that
/// `--help` for `proxies unpin` describes a general operation
/// ("Clear a pinned selection") that only exists on `url-test`/`fallback`.
#[test]
fn observed_unpin_clears_a_url_test_and_is_refused_by_a_selector() {
    let Some(sandbox) = Sandbox::start() else {
        eprintln!("no core at {CORE}; skipped");
        return;
    };
    assert_eq!(
        sandbox
            .run(&["proxies", "select", "grp-url", "node-b"])
            .code,
        0,
        "pinning a url-test group has to work for the rest to mean anything"
    );
    let cleared = sandbox.run(&["proxies", "unpin", "grp-url"]);
    assert_eq!(cleared.code, 0, "{}", cleared.said());

    let selector = sandbox.run(&["proxies", "unpin", "grp-select"]);
    assert_ne!(selector.code, 0, "a Selector has no pin to clear");
    assert!(
        selector.said().contains("grp-select"),
        "the refusal has to name what it refused: {:?}",
        selector.said()
    );
}

// ======================================= 2. the renderers

/// CLAIM (`crates/cvt-tui/src/ui/logs.rs`): "A log view is read from the
/// bottom, which is why following is the default"; `render` takes
/// `app.log_window(inner.height)` and says "The window already ends where
/// following stopped".
///
/// The window is measured in **lines** and the paragraph is rendered with
/// `Wrap { trim: false }`, so a line wider than the pane costs more than one
/// row and the window's *tail* — the newest lines, the ones following exists to
/// show — is pushed off the bottom of the pane. The pane shows the oldest of
/// the lines it asked for and nothing else.
///
/// The input is the ordinary case, not an extreme one: `mihomo`'s own log lines
/// are longer than 80 columns, and every line here is 60 characters of message
/// plus a timestamp and a level.
/// Observed failing at `f759335`; the working tree fixes it (`ui/logs.rs`), and this is the///
/// guard for the simple half of it — the sweep below is the other half.
#[test]
fn defect_the_logs_screen_hides_the_newest_lines_when_a_line_wraps() {
    let mut wrong: Vec<String> = Vec::new();
    for (width, height) in [(80u16, 24u16), (120, 40), (200, 60)] {
        // One column wider than the pane's inner width, so the claim is tested
        // at the smallest message that can wrap rather than at a length chosen
        // to fail.
        let message = "x".repeat(usize::from(width));
        let mut app = empty(Screen::Logs);
        for index in 0..40 {
            let _ = app.on_event(Event::Data(Data::Log(LogRow::new(
                "info",
                format!("line-{index:02} {message}"),
            ))));
        }
        let text = flat(&draw(&app, width, height).join("\n"));
        if !text.contains("line-39") {
            wrong.push(format!(
                "{width}x{height}: the newest line is not on screen; the pane shows the \
                 oldest of its window instead"
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "a following log pane that does not show the newest line: {wrong:?}"
    );
}

/// CLAIM (`crates/cvt-tui/src/ui/mod.rs`, `confirm`): the popup asks a question
/// and offers "[y] yes / [n] no".
///
/// The popup is a fixed five rows and the body is `question`, a blank line and
/// the two buttons, wrapped. A question that wraps to two rows therefore costs
/// the buttons their place, and the overlay that guards every destructive
/// action on this screen — `confirm_question` builds it from the profile's own
/// name, which comes from a subscription — becomes a question with no answer.
///
/// Generated rather than hand-picked: the question is built the way
/// `App::confirm_question` builds it, for every profile-name length, and the
/// buttons have to be on screen for all of them.
/// Observed failing at `f759335`; the working tree fixes it (`ui/mod.rs::confirm`), and this is///
/// the guard for the ASCII half of it.
#[test]
fn defect_the_confirm_overlay_loses_its_buttons_when_the_question_wraps() {
    let mut missing: Vec<String> = Vec::new();
    for length in 1..=90usize {
        let name = "n".repeat(length);
        let mut app = empty(Screen::Profiles);
        app.overlay = Some(Overlay::Confirm {
            // The format `App::confirm_question` uses for `Action::DeleteProfile`.
            question: format!("delete `{name}` and its document?"),
            action: Action::DeleteProfile,
        });
        let text = flat(&draw(&app, 120, 40).join("\n"));
        if !text.contains("[y] yes") || !text.contains("[n] no") {
            missing.push(format!("a {length}-character name"));
        }
    }
    assert!(
        missing.is_empty(),
        "the confirmation's yes/no buttons are off the popup for {} of 90 profile \
         names (first: {}); the popup is five rows and the question wraps",
        missing.len(),
        missing.first().map_or("-", String::as_str)
    );
}

/// CLAIM (`crates/cvt-tui/src/ui/mod.rs`, `prompt`): "The caret is drawn as a
/// selected cell rather than as a character, so a space in the middle of an
/// answer is still visible."
///
/// The line is one `Paragraph` with no horizontal scroll, so once the value is
/// wider than the popup the caret cell — and the end of the value — are past the
/// right edge. The user typing a subscription URL, which is what the prompt is
/// for, cannot see what they are typing or where they are typing it.
///
/// The caret is the only thing on that screen with a background colour
/// (`Theme::selection`), so "a cell with that background is on screen" is the
/// same statement as "the caret is drawn".
/// Observed failing at `f759335`; the working tree fixes it (`ui/mod.rs::prompt`), and this is///
/// the guard for the ASCII half of it.
#[test]
fn defect_the_prompt_caret_disappears_when_the_value_is_wider_than_the_popup() {
    let theme = Theme::default();
    let caret = theme.selection();
    let mut missing: Vec<String> = Vec::new();
    for length in [1usize, 8, 16, 40, 64, 68, 69, 96, 200] {
        let mut app = app_with(theme);
        app.screen = Screen::Profiles;
        app.overlay = Some(Overlay::Prompt {
            label: "subscription URL".to_owned(),
            kind: PromptKind::Text,
            value: "u".repeat(length),
            cursor: length,
        });
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("test backend");
        terminal
            .draw(|frame| cvt_tui::ui::render(frame, &app))
            .expect("draw");
        let drawn = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .any(|cell| cell.bg == caret.bg.unwrap_or(Color::Reset));
        if !drawn {
            missing.push(format!("{length}"));
        }
    }
    assert!(
        missing.is_empty(),
        "the caret is not drawn at all for a value of these lengths: {missing:?} \
         (the popup's inner width is 70, and nothing scrolls)"
    );
}

/// CLAIM (the in-flight fix in `crates/cvt-tui/src/ui/logs.rs`): "the window is
/// taken generously and then trimmed from the front until it fits", where the
/// rows a line needs are measured with `wrapped_rows(&format!("{at} {level}
/// {message}"), width)`.
///
/// The renderer draws a different string: `format!("{at} ")`,
/// `format!("{level:<7} ")` and the message, so a four-character level costs
/// **seven** columns when drawn and four when measured. Three columns short is
/// enough to be one row short at a wrap boundary, and one row short is one line
/// too many in the pane — which clips the bottom, which is where the newest
/// lines are.
///
/// Generated over every message length rather than at one chosen length: the
/// defect is only visible at the lengths that sit just under a boundary, and
/// hand-picking a length is how the first fix missed them.
#[test]
fn defect_the_log_window_measures_a_shorter_row_than_it_draws() {
    let mut wrong: Vec<String> = Vec::new();
    for (width, height) in [(40u16, 12u16), (80, 24), (120, 40)] {
        for message in 1..=(usize::from(width) * 2) {
            let mut app = empty(Screen::Logs);
            for index in 0..40 {
                let _ = app.on_event(Event::Data(Data::Log(LogRow::new(
                    "info",
                    format!("line-{index:02} {}", "x".repeat(message.saturating_sub(9))),
                ))));
            }
            let text = flat(&draw(&app, width, height).join("\n"));
            if !text.contains("line-39") {
                wrong.push(format!("{width}x{height} message={message}"));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "the newest line is off the pane for {} message lengths, starting at {:?} \
         (measured as `at level message`, drawn as `at level<pad7> message`)",
        wrong.len(),
        wrong.first().map_or("-", String::as_str)
    );
}

/// CLAIM (`crates/cvt-tui/src/ui/mod.rs`, the in-flight `confirm` fix): the
/// popup is "tall enough for the question at *this* width", with
/// `wrapped_rows(question, inner)`.
///
/// `wrapped_rows` counts `word.chars().count()`; a terminal counts columns, and
/// a CJK character is two of them. A name of thirteen Chinese characters is
/// twenty-six columns, is counted as thirteen, and gets a popup sized for half
/// the question — so `[y] yes / [n] no` is off the bottom again, for exactly the
/// subscriptions this program is most likely to be pointed at.
///
/// `unicode-width` is already a dependency of this crate, which is how the
/// command-line side measures the same text.
///
/// Generated over name lengths and widths: every CJK name from thirteen
/// characters up loses its buttons at every width tried, and no ASCII or
/// space-separated name does, which is what makes this a measurement defect
/// rather than a sizing one.
#[test]
fn defect_the_confirmation_popup_is_sized_in_characters_not_columns() {
    let mut wrong: Vec<String> = Vec::new();
    for width in [60u16, 80, 120, 200] {
        for characters in 1..=60usize {
            let name = "日".repeat(characters);
            let mut app = empty(Screen::Profiles);
            app.overlay = Some(Overlay::Confirm {
                question: format!("delete `{name}` and its document?"),
                action: Action::DeleteProfile,
            });
            let text = flat(&draw(&app, width, 40).join("\n"));
            if !text.contains("[y] yes") || !text.contains("[n] no") {
                wrong.push(format!("{width} wide, {characters} character(s)"));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "a confirmation with no buttons, for {} of 240 names, from {:?}",
        wrong.len(),
        wrong.first().map_or("-", String::as_str)
    );
}

/// CLAIM (`crates/cvt-tui/src/ui/mod.rs`, the in-flight `prompt` fix): "The
/// window follows the caret", with `inner = width - 4` cells of the value kept
/// around it.
///
/// The window is a count of `char`s and the popup is a count of columns, so a
/// value of wide characters puts the caret past the popup's right edge again —
/// `cursor` is a character index and `inner` is a column budget, and nothing
/// reconciles them. The failure is the original one, for a CJK value: the caret
/// cell is not drawn at all.
///
/// Generated over widths, value lengths and caret positions, and over ASCII as
/// the control: every ASCII case passes and every failing case has wide
/// characters in it.
#[test]
fn defect_the_prompt_window_is_measured_in_characters_not_columns() {
    let theme = Theme::default();
    let caret_style = theme.selection();
    let mut wrong: Vec<String> = Vec::new();
    for width in [40u16, 60, 80, 120] {
        for length in [1usize, 10, 50, 69, 100, 200] {
            for cursor in [0usize, length / 3, length / 2, length] {
                for (tag, fill) in [("ascii", "u"), ("wide", "日")] {
                    let value: String = std::iter::repeat_n(fill, length).collect();
                    let mut app = app_with(theme);
                    app.screen = Screen::Profiles;
                    app.overlay = Some(Overlay::Prompt {
                        label: "subscription URL".to_owned(),
                        kind: PromptKind::Text,
                        value,
                        cursor: cursor.min(length),
                    });
                    let mut terminal = Terminal::new(TestBackend::new(width, 24)).expect("backend");
                    terminal
                        .draw(|frame| cvt_tui::ui::render(frame, &app))
                        .expect("draw");
                    let drawn = terminal
                        .backend()
                        .buffer()
                        .content()
                        .iter()
                        .any(|cell| cell.bg == caret_style.bg.unwrap_or(Color::Reset));
                    if !drawn {
                        wrong.push(format!("{width} {tag} len={length} cursor={cursor}"));
                    }
                }
            }
        }
    }
    let wide = wrong.iter().filter(|line| line.contains("wide")).count();
    assert!(
        wrong.is_empty(),
        "the caret is not drawn for {} of 192 cases, {wide} of them with wide \
         characters: {:?}",
        wrong.len(),
        &wrong[..wrong.len().min(4)]
    );
}

/// CLAIM (`crates/cvt-tui/src/ui/mod.rs`, `prompt`): the caret is drawn as a
/// selected cell, "so a space in the middle of an answer is still visible".
///
/// The cell's content is `chars.get(cursor)`, and a character with no width of
/// its own is not drawn by ratatui at all — so a caret that lands on it styles
/// nothing and the cursor disappears while the user is editing. The position is
/// reachable with one keypress: text in decomposed form (NFD, which is what
/// macOS and several input methods produce) puts a combining mark immediately
/// after its base character, and the cursor is legitimately between them.
///
/// **Pre-existing rather than a regression of the fix above**: the version at
/// `f759335` was `let under = chars.get(cursor).copied().unwrap_or(' ')` and
/// did the same thing. It is here because the sweep for the fix found it.
///
/// The dump that settles it, at 40x12 with the value `e\u{301}`:
///
/// ```text
/// cursor=1 (before the mark): styled cells = []      <-- no caret at all
/// cursor=0 (on the `e`):      styled cells = ["e"]   <-- the caret, as intended
/// ```
///
/// Generated over value lengths and **every** cursor position, so the claim is
/// about the class "the caret is on a combining mark" rather than about one
/// string.
#[test]
fn defect_the_caret_is_not_drawn_when_it_sits_on_a_combining_mark() {
    let theme = Theme::default();
    let caret_style = theme.selection();
    let mut wrong: Vec<String> = Vec::new();
    for width in [20u16, 40, 80] {
        for pairs in [1usize, 5, 20] {
            let value = "e\u{301}".repeat(pairs);
            let characters = value.chars().count();
            for cursor in 0..=characters {
                let mut app = app_with(theme);
                app.screen = Screen::Profiles;
                app.overlay = Some(Overlay::Prompt {
                    label: "subscription URL".to_owned(),
                    kind: PromptKind::Text,
                    value: value.clone(),
                    cursor,
                });
                let mut terminal = Terminal::new(TestBackend::new(width, 12)).expect("backend");
                terminal
                    .draw(|frame| cvt_tui::ui::render(frame, &app))
                    .expect("draw");
                let drawn = terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .any(|cell| cell.bg == caret_style.bg.unwrap_or(Color::Reset));
                if !drawn {
                    wrong.push(format!("{width} pairs={pairs} cursor={cursor}"));
                }
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "the caret is not drawn for {} of 156 cursor positions, every one of them \
         on a combining mark: {:?}",
        wrong.len(),
        &wrong[..wrong.len().min(4)]
    );
}

/// CLAIM (`crates/cvt-tui/src/ui/render_tests.rs`, module doc): "Every screen is
/// drawn empty *and* populated, at four sizes including the degenerate one."
///
/// Checked rather than trusted, and gone past: a terminal of **zero** columns
/// and zero rows is what a pty reports before its size is set — measured, the
/// interface draws nothing and stays alive, which is the right answer and is
/// asserted here for every screen at every size the existing test does not use.
#[test]
fn confirmed_every_screen_draws_empty_and_populated_including_a_terminal_of_zero() {
    let sizes = [
        (0u16, 0u16),
        (0, 24),
        (80, 0),
        (1, 1),
        (1, 40),
        (200, 1),
        (13, 7),
    ];
    let screens = Screen::all();
    // The enumeration, held to the class: a tenth screen fails here.
    assert_eq!(
        screens.len(),
        9,
        "the interface has gained a screen; this test's sizes and the empty-state \
         claim below have to be extended with it"
    );
    for screen in screens {
        for with_rows in [false, true] {
            let app = if with_rows {
                populated(screen)
            } else {
                empty(screen)
            };
            for (width, height) in sizes {
                let rows = draw(&app, width, height);
                assert_eq!(
                    rows.len(),
                    usize::from(height),
                    "{screen} (with_rows={with_rows}) at {width}x{height}"
                );
                for row in &rows {
                    assert_eq!(
                        row.chars().count(),
                        usize::from(width),
                        "{screen} (with_rows={with_rows}) at {width}x{height}: {row:?} \
                         is not the terminal's width"
                    );
                }
            }
        }
    }
}

/// The empty-state claim of the existing suite covers five of the nine screens.
///
/// `render_tests::an_empty_screen_explains_itself_rather_than_drawing_a_blank_box`
/// carries a five-row table — Profiles, Proxies, Connections, Rules, Logs — and
/// has no entry for Home, Tests, Settings or Help. This is the class version:
/// **every** screen, with nothing loaded, has to put a non-trivial message on
/// screen. The four the table omits do say something (Home: "the core is not
/// running — press s to start it"; Tests: the four pending checks; Settings:
/// twenty-three rows; Help: the key reference), so this is a confirmed result
/// and a note for the next round rather than a finding.
#[test]
fn confirmed_every_screen_says_something_when_it_has_nothing() {
    let mut wordless: Vec<String> = Vec::new();
    for screen in Screen::all() {
        let text = flat(&draw(&empty(screen), 120, 40).join("\n"));
        // The tab bar and the footer are always drawn; the screen's own pane
        // has to contribute words of its own. Twenty characters past the chrome
        // is a deliberately low bar: it catches a pane that drew a border and
        // nothing else, which is the failure the helper exists to prevent.
        if text.len() < 120 {
            wordless.push(format!("{screen}: {text:?}"));
        }
    }
    assert!(
        wordless.is_empty(),
        "screens that drew (almost) nothing when they had nothing to draw: {wordless:?}"
    );
}

/// A name of 4000 characters, and a row count of 5000, at every size.
///
/// The claim the frame rests on is that a row is the terminal's width and never
/// wider — the buffer's own rows are counted here rather than the text being
/// eyeballed, so a long name cannot quietly write past the right edge.
#[test]
fn confirmed_a_four_thousand_character_name_stays_inside_the_frame() {
    let long = "N".repeat(4000);
    let mut app = populated(Screen::Proxies);
    let _ = app.on_event(Event::Data(Data::Nodes(vec![
        NodeRow {
            name: long.clone(),
            kind: "Vless".to_owned(),
            group: Some(long.clone()),
            delay: Some(20),
            alive: true,
            active: true,
            is_group: true,
            is_proxy: false,
            members: 4000,
            selectable: true,
        },
        NodeRow {
            name: "日本 東京 ノード 01 — ünïcödé ✓".to_owned(),
            kind: "日本".to_owned(),
            group: Some("grp".to_owned()),
            delay: None,
            alive: false,
            active: false,
            is_group: false,
            is_proxy: false,
            members: 0,
            selectable: false,
        },
    ])));
    let rows: Vec<ConnectionRow> = (0..5000)
        .map(|index| ConnectionRow {
            id: format!("c{index}"),
            destination: format!("host{index}.example.com:443"),
            network: "tcp".to_owned(),
            process: "curl".to_owned(),
            rule: "MATCH".to_owned(),
            chain: "DIRECT".to_owned(),
            upload: 1,
            download: 2,
            started: "10:00:00".to_owned(),
        })
        .collect();
    let _ = app.on_event(Event::Data(Data::Connections(rows)));

    for screen in Screen::all() {
        app.screen = screen;
        for (width, height) in [(1u16, 1u16), (40, 10), (80, 24), (200, 60)] {
            let drawn = draw(&app, width, height);
            assert_eq!(
                drawn.len(),
                usize::from(height),
                "{screen} at {width}x{height}"
            );
            for row in &drawn {
                assert_eq!(
                    row.chars().count(),
                    usize::from(width),
                    "{screen} at {width}x{height}: {row:?}"
                );
            }
        }
    }
}

// ======================================= 3. `run.rs`, with a real terminal

/// The driver a real terminal needs, written out because the test crate cannot
/// spawn a pty from `std` alone.
///
/// It allocates a pty, sets its window size to 40x120, runs the binary in it,
/// reads the termios **while the interface is running** and again after it has
/// exited, and prints one JSON object.
const PTY_DRIVER: &str = r#"
import fcntl, json, os, pty, re, select, signal, struct, sys, termios, time

binary, home, env_mode, keys = sys.argv[1:5]
args = sys.argv[5:]

pid, fd = pty.fork()
if pid == 0:
    os.environ["TERM"] = "xterm-256color"
    if env_mode == "clean":
        os.environ.pop("NO_COLOR", None)
    else:
        os.environ["NO_COLOR"] = "1"
    os.execv(binary, [binary, "--home", home] + args)

fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
out = b""

def drain(seconds, stop_on=None):
    global out
    end = time.time() + seconds
    while time.time() < end:
        ready, _, _ = select.select([fd], [], [], 0.05)
        if not ready:
            continue
        try:
            chunk = os.read(fd, 65536)
        except OSError:
            return
        if not chunk:
            return
        out += chunk
        if stop_on is not None and stop_on in out:
            return

drain(5.0, b"Home")
drain(0.5)
for key in keys:
    os.write(fd, key.encode("ascii"))
    drain(0.3)
before = termios.tcgetattr(fd)
os.write(fd, b"q")
code = None
end = time.time() + 5.0
while time.time() < end:
    waited, status = os.waitpid(pid, os.WNOHANG)
    if waited == pid:
        code = status
        break
    drain(0.05)
if code is None:
    os.kill(pid, signal.SIGKILL)
    _, code = os.waitpid(pid, 0)
drain(0.2)
after = termios.tcgetattr(fd)

COLOR = re.compile(rb"\x1b\[((?:3[0-8]|4[0-8]|9[0-8]|38;5;\d+|48;5;\d+));?[0-9]*m")
print(json.dumps({
    "exit": os.waitstatus_to_exitcode(code),
    "bytes": len(out),
    "drew": b"Home" in out,
    "colour_sequences": len(COLOR.findall(out)),
    "alt_enter": b"\x1b[?1049h" in out,
    "alt_leave": b"\x1b[?1049l" in out,
    "mouse_enter": b"\x1b[?1006h" in out and b"\x1b[?1000h" in out,
    "mouse_leave": b"\x1b[?1006l" in out and b"\x1b[?1000l" in out,
    "missing_controller": b"is missing required field" in out,
    "empty_proxies": b"no proxies yet" in out,
    "empty_connections": b"no connections" in out,
    "empty_rules": b"no rules" in out,
    "icanon_while_running": bool(before[3] & termios.ICANON),
    "icanon_after_exit": bool(after[3] & termios.ICANON),
    "echo_after_exit": bool(after[3] & termios.ECHO),
}))
"#;

/// Run the interface under a real pty and return the driver's JSON object.
fn under_a_terminal(home: &Path, args: &[&str], no_color_env: bool) -> serde_json::Value {
    under_a_terminal_with_keys(home, args, no_color_env, "")
}

fn under_a_terminal_with_keys(
    home: &Path,
    args: &[&str],
    no_color_env: bool,
    keys: &str,
) -> serde_json::Value {
    let dir = TempDir::new().expect("a temporary directory");
    let script = dir.path().join("pty_driver.py");
    std::fs::write(&script, PTY_DRIVER).expect("the driver is written");
    let mut command = Command::new("python3");
    command
        .arg(&script)
        .arg(cvt_binary())
        .arg(home)
        .arg(if no_color_env { "nocolor" } else { "clean" })
        .arg(keys)
        .args(args);
    let out = command.output().expect("python3 runs the driver");
    assert!(
        out.status.success(),
        "the pty driver failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|error| {
        panic!(
            "the pty driver printed no JSON ({error}): {:?}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

/// The terminal scope, settled with a terminal.
///
/// `run.rs` claims two things about it: it takes the terminal over (raw mode,
/// alternate screen) and it does not return without restoring it. Nothing in
/// this repository has ever given it one, so the claim is checked where the
/// terminal's own termios is the witness: `ICANON` is off while the interface
/// is running and back on after it has exited, and the alternate screen was
/// entered and left.
#[test]
fn confirmed_the_terminal_is_taken_over_and_given_back() {
    let dir = TempDir::new().expect("a temporary directory");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).expect("the home");
    let report = under_a_terminal(&home, &[], false);
    assert_eq!(report["exit"], 0, "{report}");
    assert_eq!(report["drew"], true, "the interface never drew: {report}");
    assert_eq!(
        report["icanon_while_running"], false,
        "the interface did not put the terminal into raw mode: {report}"
    );
    assert_eq!(
        report["icanon_after_exit"], true,
        "raw mode was left on after the interface exited: {report}"
    );
    assert_eq!(report["echo_after_exit"], true, "{report}");
    assert_eq!(report["alt_enter"], true, "{report}");
    assert_eq!(report["alt_leave"], true, "{report}");
    assert_eq!(report["mouse_enter"], true, "{report}");
    assert_eq!(report["mouse_leave"], true, "{report}");
    assert_eq!(report["missing_controller"], false, "{report}");
}

#[test]
fn empty_home_can_visit_every_controller_screen_without_a_missing_field_error() {
    let dir = TempDir::new().expect("a temporary directory");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).expect("the home");
    // 3, 4 and 6 visit Proxies, Connections and Rules respectively. Capturing
    // every frame catches even an error hidden by a later screen's status.
    let report = under_a_terminal_with_keys(&home, &[], false, "346");
    assert_eq!(report["exit"], 0, "{report}");
    assert_eq!(report["empty_proxies"], true, "{report}");
    assert_eq!(report["empty_connections"], true, "{report}");
    assert_eq!(report["empty_rules"], true, "{report}");
    assert_eq!(report["missing_controller"], false, "{report}");
}

/// CLAIM (`docs/CLI.md`): "`--no-color` | Never emit ANSI colour. `NO_COLOR` in
/// the environment does the same."
///
/// The two are not the same. `main` folds `--no-color` and `NO_COLOR` into one
/// `color` flag and hands it to `Output`, which is what the *commands* render
/// through; the interface path (`crate::tui::run`) builds its theme from
/// `service.settings().ui.color` alone and never sees it. `NO_COLOR` reaches
/// the interface by a different route — `crossterm` honours it inside its own
/// colour commands — so the environment variable removes every colour from the
/// interface while the flag that documents itself as doing the same removes
/// none.
///
/// Measured, with `NO_COLOR` unset in the child:
///
/// ```text
/// cvt                    -> 36 colour sequences
/// cvt --no-color         -> 36 colour sequences
/// NO_COLOR=1 cvt         ->  0 colour sequences
/// ```
/// Observed failing at `f759335`; the working tree fixes it (`cvt/src/tui.rs` passes the flag///
/// to `Theme::from_settings`, `cvt/src/output.rs` exposes it) and this is the guard.
#[test]
fn defect_the_interface_colours_its_output_despite_no_color() {
    let dir = TempDir::new().expect("a temporary directory");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).expect("the home");

    let plain = under_a_terminal(&home, &[], false);
    let flag = under_a_terminal(&home, &["--no-color"], false);
    let environment = under_a_terminal(&home, &[], true);

    // The control: without either, the interface is coloured, so the counter
    // below is counting something that is really there.
    assert!(
        plain["colour_sequences"].as_u64().unwrap_or(0) > 0,
        "the control run emitted no colour, so this test proves nothing: {plain}"
    );
    assert_eq!(
        environment["colour_sequences"], 0,
        "`NO_COLOR` no longer reaches the interface: {environment}"
    );
    assert_eq!(
        flag["colour_sequences"], 0,
        "`--no-color` did not reach the interface: {} colour sequences still \
         emitted, against {} for the environment variable",
        flag["colour_sequences"], environment["colour_sequences"]
    );
}

/// CLAIM (`crates/cvt/src/tui.rs`): "the interface must fail loudly rather than
/// write escape sequences into a pipe and report success."
///
/// Asserted at the binary, with stdout a pipe: nothing on stdout, a message
/// naming the terminal on stderr, and a non-zero exit.
#[test]
fn confirmed_a_pipe_is_refused_rather_than_written_into() {
    let dir = TempDir::new().expect("a temporary directory");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).expect("the home");
    let ran = run(&home, &[]);
    assert_ne!(
        ran.code, 0,
        "the interface reported success without a terminal"
    );
    assert!(
        ran.stdout.is_empty(),
        "the interface wrote {:?} into a pipe",
        ran.stdout
    );
    assert!(
        ran.said().contains("terminal"),
        "the message has to say what is missing: {:?}",
        ran.said()
    );
}

/// RECORD: what `--name ''` does — and a claim I could not settle.
///
/// `AddArgs::name` documents "Display name; defaults to the host of the URL".
/// At `f759335` one run of
///
/// ```text
/// $ cvt profiles add http://127.0.0.1:9/x --name ''      # a closed port, no network
/// $ cat $HOME/profiles.yaml
/// - uid: RhbtkUrshVNf
///
///  type: remote
///
///  name: ''                    <-- the empty name, stored
/// ```
///
/// stored the empty string, which is neither the host the help promises nor a
/// name anybody chose, and `--json profiles list` then showed the uid where the
/// name belongs. **It does not reproduce.** At `d79a4f2` the same command, run
/// three times on fresh homes and once with the argument order swapped, writes
/// `name: 127.0.0.1` every time — and the source on that path (`AddArgs`,
/// `profiles::add`, `default_name`) is byte-identical between the two commits,
/// so this is my own first observation being wrong rather than a behaviour that
/// changed. It is recorded instead of being asserted, and the assertion below
/// is the reproducible half.
#[test]
fn observed_an_empty_name_falls_back_to_the_host() {
    let dir = TempDir::new().expect("a temporary directory");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).expect("the home");
    let ran = run(
        &home,
        &["profiles", "add", "http://127.0.0.1:9/x", "--name", ""],
    );
    let index = std::fs::read_to_string(home.join("profiles.yaml")).unwrap_or_default();
    assert!(
        index.contains("name: 127.0.0.1"),
        "the empty `--name` did not fall back to the host (exit {}):\n{index}",
        ran.code
    );
    assert!(
        !index.contains("name: ''"),
        "the profile's name is the empty string:\n{index}"
    );
}

// ======================================= 4. the global flags

/// CLAIM (`docs/CLI.md`): "`--home <DIR>` | Application home. Overrides
/// `CVT_HOME`; the platform data directory is the fallback." and "`CVT_HOME` |
/// Application home, used when `--home` is absent."
///
/// All three rungs, settled from the binary's own `--json` report. The empty
/// `CVT_HOME` case is the interesting one: `AppPaths::resolve` filters an empty
/// environment value but, being handed `Some("")` by the flag parser, would not
/// — and `clap` refuses `--home ''` before it gets there, so the filter is what
/// keeps an exported-but-empty variable from silently making the home the
/// current directory.
#[test]
fn confirmed_the_home_flag_beats_the_environment_and_the_environment_is_honoured() {
    let dir = TempDir::new().expect("a temporary directory");
    let from_env = dir.path().join("env-home");
    let from_flag = dir.path().join("flag-home");
    let data = dir.path().join("data");

    let json_home = |args: &[&str], envs: &[(&str, &str)]| -> (i32, String) {
        let mut command = Command::new(cvt_binary());
        command.args(["--json"]).args(args);
        for (key, value) in envs {
            command.env(key, value);
        }
        let out = command.output().expect("the binary runs");
        let value: serde_json::Value =
            serde_json::from_slice(&out.stdout).unwrap_or_else(|error| {
                panic!(
                    "no JSON from {args:?} ({error}): {:?}",
                    String::from_utf8_lossy(&out.stdout)
                )
            });
        (
            out.status.code().unwrap_or(-1),
            value["home"].as_str().unwrap_or("-").to_owned(),
        )
    };

    let (code, home) = json_home(&["status"], &[("CVT_HOME", from_env.to_str().unwrap())]);
    assert_eq!(code, 0, "`CVT_HOME` alone did not work");
    assert_eq!(
        home,
        from_env.display().to_string(),
        "`CVT_HOME` was ignored"
    );

    let (code, home) = json_home(
        &["--home", from_flag.to_str().unwrap(), "status"],
        &[("CVT_HOME", from_env.to_str().unwrap())],
    );
    assert_eq!(code, 0);
    assert_eq!(
        home,
        from_flag.display().to_string(),
        "`--home` has to override `CVT_HOME`"
    );

    // An empty variable is not a home. `XDG_DATA_HOME` is where the platform
    // fallback lands, so the answer is checkable without knowing the machine.
    let (code, home) = json_home(
        &["status"],
        &[
            ("CVT_HOME", ""),
            ("XDG_DATA_HOME", data.to_str().unwrap()),
            ("HOME", data.to_str().unwrap()),
        ],
    );
    assert_eq!(code, 0, "an empty `CVT_HOME` should fall back, not fail");
    assert_eq!(
        home,
        data.join("clash-verge-tui").display().to_string(),
        "an empty `CVT_HOME` was taken as a home"
    );
}

/// CLAIM (`crates/cvt/src/output.rs`, module doc): "stdout carries the result
/// and nothing else... so `cvt ... --json | jq` never has to filter a chatty
/// diagnostic out of the payload", and `cvt/src/main.rs`: diagnostics never
/// land on the stdout that `--json` output is piped from.
///
/// Checked with `-vv`, which is the loudest the program gets, and on a
/// command that fails as well as one that succeeds: stdout parses as exactly
/// one JSON object with a `schema`, and the diagnostics are on stderr.
#[test]
fn confirmed_json_keeps_stdout_machine_readable() {
    let dir = TempDir::new().expect("a temporary directory");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).expect("the home");
    for args in [
        vec!["--json", "-vv", "status"],
        vec!["--json", "-vv", "profiles", "list"],
        vec!["--json", "proxies", "list", "no-such"],
        vec!["--json", "logs", "--level", "bogus"],
    ] {
        let ran = run(&home, &args);
        assert!(
            !ran.stdout.contains("error:"),
            "{args:?}: a diagnostic reached stdout: {:?}",
            ran.stdout
        );
        if ran.stdout.trim().is_empty() {
            // A command that failed before it built a report — here, one with
            // no profile to take a controller address from — prints nothing,
            // which is not the same as printing prose. It still has to fail.
            assert_ne!(
                ran.code, 0,
                "{args:?} printed no report and still reported success"
            );
            assert!(
                !ran.stderr.trim().is_empty(),
                "{args:?} failed without saying why"
            );
            continue;
        }
        let value: serde_json::Value =
            serde_json::from_str(ran.stdout.trim()).unwrap_or_else(|error| {
                panic!(
                    "{args:?}: stdout is not one JSON object ({error}): {:?}",
                    ran.stdout
                )
            });
        assert!(
            value["schema"]
                .as_str()
                .is_some_and(|s| s.starts_with("cvt.")),
            "{args:?}: no `schema` field: {:?}",
            ran.stdout
        );
    }
}

/// The per-command flags, enumerated from the binary's own `--help`.
///
/// The tenth review spot-checked this class rather than enumerating it. Every
/// long flag every leaf command declares is collected here, minus the four
/// globals, and compared with a table — so a new per-command flag fails this
/// test with "extend the table" instead of being a flag nobody looked at. The
/// table carries the answer each one gave, which is the half that cannot be
/// derived.
///
/// | flag | command | answer |
/// |---|---|---|
/// | `--all` | `connections close`, `test delay` | required by an `ArgGroup` |
/// | `--all-due` | `profiles update` | conflicts with a uid (exit 2) |
/// | `--apply` | `config generate` | gates `--force` and `--mode` |
/// | `--channel` | `core upgrade` | `ValueEnum`, exit 2 on a typo |
/// | `--clear` | `profiles chain` | conflicts with uids (exit 2) |
/// | `--concurrency` | `proxies test`, `proxies test-all`, `test delay`, `test urls` | refused at 0 |
/// | `--direct` | `geo` | a switch |
/// | `--disabled` | `rules list` | a switch |
/// | `--filter` | `logs` | free text, no validation |
/// | `--follow` | `logs` | a switch |
/// | `--force` | `config generate`, `core upgrade` | requires `--apply` on the first |
/// | `--group` | `test delay` | a **name**: covered by section 1 |
/// | `--level` | `logs` | refused on a typo, lists the five levels |
/// | `--limit` | `connections list` | `0` documented as "no limit" |
/// | `--lines` | `logs` | `0` documented as "everything" |
/// | `--list` | `test urls` | a switch |
/// | `--mode` | `config generate` | `ValueEnum`, exit 2 on a typo |
/// | `--name` | `profiles add` | free text; empty is refused by clap |
/// | `--no-fetch` | `profiles edit-url` | a switch |
/// | `--node` | `test urls` | a **name**: covered by section 1 |
/// | `--stats` | `rules list` | a switch |
/// | `--timeout` | `geo`, `unlock`, `proxies test`, `proxies test-all`, `test delay`, `test urls` | refused at 0 and above the ceiling |
/// | `--type` | `test dns` | passed to the core, which owns the list |
/// | `--url` | `proxies test`, `proxies test-all`, `test delay` | refused when not a URL or a known target |
#[test]
fn confirmed_the_per_command_flag_class_is_exactly_these_names() {
    let mut declared: BTreeSet<String> = BTreeSet::new();
    for leaf in leaf_commands() {
        let page = help(&leaf);
        for line in page.lines() {
            for token in line.split_whitespace() {
                if let Some(flag) = token.strip_prefix("--") {
                    let name = flag
                        .split(['=', '<'])
                        .next()
                        .unwrap_or(flag)
                        .trim_end_matches("...");
                    if !matches!(
                        name,
                        "home" | "json" | "verbose" | "no-color" | "help" | "version"
                    ) {
                        declared.insert(name.to_owned());
                    }
                }
            }
        }
    }
    let expected: BTreeSet<String> = [
        "all",
        "all-due",
        "apply",
        "channel",
        "clear",
        "concurrency",
        "direct",
        "disabled",
        "filter",
        "follow",
        "force",
        "group",
        "level",
        "limit",
        "lines",
        "list",
        "mode",
        "name",
        "no-fetch",
        "node",
        "stats",
        "timeout",
        "type",
        "url",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let joined: Vec<String> = declared.difference(&expected).cloned().collect();
    let missing: Vec<String> = expected.difference(&declared).cloned().collect();
    assert!(
        joined.is_empty() && missing.is_empty(),
        "the per-command flag class moved: these joined and are unchecked {joined:?}; \
         these are gone from --help {missing:?}"
    );
}

/// RECORD: the per-command flags that were enumerated and checked by hand, and
/// the ones that are still only spot-checked.
///
/// This is the semi-mechanical half of the flag class, kept in the file so the
/// next round can see which members were looked at and what each answered:
///
///
/// | flag | command | answer |
/// |---|---|---|
/// | `--lines 0` | `logs` | documented as "everything" |
/// | `--lines` huge | `logs` | no refusal |
/// | `--limit 0` | `connections list` | documented as "no limit" |
/// | `--level bogus` | `logs` | refused, lists the valid levels |
/// | `--level WARNING` | `logs` | accepted (case-folded) |
/// | `--type bogus` | `test dns` | passed to the core, not checked here |
/// | `--timeout 0` | `geo`, `unlock` | refused, "outside the range 1 to 120000" |
/// | `--timeout 0` | `proxies test` | refused, "outside the range the core accepts" |
/// | `--timeout 32768` | `proxies test` | refused, "the core parses this as an int16" |
/// | `--concurrency 0` | `proxies test` | refused, "must be at least 1" |
/// | `--force` without `--apply` | `config generate` | refused by clap, exit 2 |
/// | `--clear` with uids | `profiles chain` | refused by clap, exit 2 |
/// | `--all-due` with a uid | `profiles update` | refused by clap, exit 2 |
/// | `--name` | `profiles add` | accepted; the name is not validated |
/// | `--all` | `proxies test-all` | the only scope it has |
#[test]
fn observed_the_per_command_flags_that_were_enumerated_by_hand() {
    let dir = TempDir::new().expect("a temporary directory");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).expect("the home");
    // The two that are not clap's answer, checked here so the table above is
    // measured rather than remembered.
    let refused = run(&home, &["logs", "--level", "bogus"]);
    assert_ne!(refused.code, 0, "an unknown level has to be refused");
    assert!(refused.said().contains("silent"), "{:?}", refused.said());
    let refused = run(&home, &["geo", "--timeout", "0"]);
    assert_ne!(refused.code, 0, "a zero timeout has to be refused");
    assert!(refused.said().contains("120000"), "{:?}", refused.said());
    // A usage error is clap's, and it is code 2.
    for args in [
        vec!["config", "generate", "--force"],
        vec!["config", "generate", "--mode", "hot"],
        vec!["profiles", "chain", "m1", "--clear"],
        vec!["profiles", "update", "R1", "--all-due"],
        vec!["connections", "close", "abc", "--all"],
        vec!["test", "delay"],
        vec!["core", "upgrade", "--channel", "beta"],
        vec!["config", "generate", "--apply", "--mode", "fast"],
    ] {
        let usage = run(&home, &args);
        assert_eq!(usage.code, 2, "{args:?}: {:?}", usage.said());
    }
    // The other command that makes its own request, held to the same ceiling
    // as `geo`.
    let refused = run(&home, &["unlock", "--timeout", "0"]);
    assert_ne!(refused.code, 0, "a zero timeout has to be refused");
    assert!(refused.said().contains("120000"), "{:?}", refused.said());
}
