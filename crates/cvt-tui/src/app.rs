//! The application state machine.
//!
//! Three types carry the whole protocol between the interface and the world:
//! a key press becomes an [`Action`] (or a mouse event changes the current
//! selection), the machine answers with a list of
//! [`Effect`]s, and the binary crate performs them and reports back with an
//! [`Event`]. Nothing here opens a file, resolves a name or spawns a process —
//! that rule is what makes the behaviour a user notices (does `q` really quit
//! while a prompt is open? does a refresh keep my place in the list?) testable
//! without a terminal, a core or a network.
//!
//! ```text
//! KeyEvent ──▶ Keymap ──▶ Action ──▶ App ──▶ Vec<Effect> ──▶ binary
//! MouseEvent ────────────────────────┘
//!                                       ◀── Event::{Data,Done,Failed}
//! ```
//!
//! # Where the decisions live
//!
//! [`App::on_event`] is the only entry point. It routes input through the
//! overlay first (so a prompt can be typed into), then keys through the key
//! map and dispatcher. The dispatcher is deliberately explicit about every
//! [`Action`]: an arm that cannot do what was asked reports *why* through
//! [`StatusKind::Warning`] instead of failing silently, because "the key did
//! nothing" is the worst answer a keyboard-driven program can give.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use cvt_core::ReloadMode;
use cvt_core::mihomo::supervisor::CoreStatus;
use cvt_core::mihomo::types::{LogLevel, Traffic};
use cvt_core::settings::{Language, Settings};

use crate::action::{Action, Screen};
use crate::keys::Keymap;
use crate::row::{
    ConnectionRow, LogRow, NodeRow, ProfileRow, RuleRow, TestKind, TestResult, TestRow,
};
use crate::state::{Filterable, LogBuffer, Metrics, SortOrder, Table, contains_ignore_case};
use crate::theme::Theme;

/// How long a transient status message stays on screen.
///
/// Long enough to read a sentence, short enough that it does not keep covering
/// the footer. The full latest message remains available through `m`.
pub const STATUS_TTL: Duration = Duration::from_secs(4);

/// How many ticks pass between background polls of the running core.
pub const POLL_EVERY: u64 = 20;

// ---------------------------------------------------------------------------
// effects
// ---------------------------------------------------------------------------

/// A side effect the binary crate must perform.
///
/// Every variant is a request, never an observation: the machine never learns
/// the outcome except through [`Event::Done`] or [`Event::Failed`]. Payloads
/// carry what the binary cannot work out on its own — which uid, which node,
/// which reload discipline — and nothing else.
// `SaveSettings` carries a whole `Settings` and is by far the largest variant.
// Boxing it would make every `Effect` match in the loop pay an indirection for
// a payload that is constructed a handful of times per keystroke; the vector of
// effects is built and dropped within one event, so the wasted bytes are
// transient rather than resident.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Run the enabled launch actions once, after the terminal is ready.
    Startup,
    /// Restore the terminal and exit. The core keeps running.
    Quit,
    /// Re-read everything a screen shows.
    Refresh(Screen),
    /// Read the profile store into [`Data::Profiles`].
    LoadProfiles,
    /// Make a profile the base document and regenerate.
    SwitchProfile {
        /// Profile uid.
        uid: String,
    },
    /// Download one or more subscriptions again. An empty list means "every
    /// remote profile that is due".
    UpdateProfiles {
        /// Profile uids, in the order they should be attempted.
        uids: Vec<String>,
    },
    /// Persist the explicit chain.
    SetChain {
        /// Patch profile uids, in application order.
        uids: Vec<String>,
    },
    /// Generate a configuration and report the difference, without applying.
    PreviewConfig,
    /// Generate, write and hand the configuration to the core.
    ApplyConfig {
        /// How the change should reach the core.
        mode: ReloadMode,
    },
    /// Generate the selected profile without starting the core.
    PrepareConfig,
    /// Restore the most recent snapshot.
    RollbackConfig,
    /// Delete a profile and its document.
    DeleteProfile {
        /// Profile uid.
        uid: String,
    },
    /// Give a profile a new name.
    RenameProfile {
        /// Profile uid.
        uid: String,
        /// The new name.
        name: String,
    },
    /// Create a profile from a URL, or a blank local one when `url` is `None`.
    NewProfile {
        /// Display name.
        name: String,
        /// Subscription URL.
        url: Option<String>,
    },
    /// Look for an existing `clash-verge-rev` home to import from.
    DetectImportSources,
    /// Copy the profiles out of another installation.
    ImportProfiles {
        /// The home directory to import from.
        source: PathBuf,
    },
    /// Pin a node inside a group.
    SelectNode {
        /// Group name.
        group: String,
        /// Member name.
        member: String,
    },
    /// Return a group to automatic selection.
    ClearNodePin {
        /// Group name.
        group: String,
    },
    /// Measure one node.
    TestNode {
        /// Node name.
        name: String,
    },
    /// Measure every member of a group.
    TestGroup {
        /// Group name.
        group: String,
    },
    /// Measure every node the core knows about.
    TestAllNodes,
    /// Run one entry of the tests screen.
    RunTest {
        /// Which check to run.
        kind: TestKind,
        /// What to run it against.
        target: String,
    },
    /// Abandon the running test batch.
    CancelTests,
    /// Forget cached latency results.
    ClearTestResults,
    /// Drop one connection.
    CloseConnection {
        /// Connection id.
        id: String,
    },
    /// Drop every connection.
    CloseAllConnections,
    /// Enable or disable one rule in the running core.
    ToggleRule {
        /// Rule position in the running configuration.
        index: u32,
        /// The state to move to.
        disabled: bool,
    },
    /// Download rule sets again. An empty list means every provider.
    UpdateRuleProviders {
        /// Provider names.
        names: Vec<String>,
    },
    /// Change the core's minimum log level.
    SetCoreLogLevel(LogLevel),
    /// Change the running core's routing mode.
    SetCoreMode {
        /// `rule`, `global` or `direct`.
        mode: String,
    },
    /// Launch the core with the generated configuration.
    StartCore,
    /// Stop the core process.
    StopCore,
    /// Stop and start the core.
    RestartCore,
    /// Download and install a newer core.
    UpgradeCore,
    /// Refresh the GeoIP and GeoSite databases.
    UpdateGeo,
    /// Clear the fake-IP and DNS caches.
    FlushCaches,
    /// Write the settings to disk.
    SaveSettings {
        /// The settings to persist.
        settings: Settings,
    },
    /// Write buffered log lines to a file.
    ExportLogs {
        /// Where to write them.
        path: PathBuf,
        /// The rendered contents.
        contents: String,
    },
    /// Open the generated runtime configuration in `$EDITOR`.
    ///
    /// The path is built from [`App::home`], mirroring `AppPaths::runtime_config`.
    OpenEditor {
        /// File to open.
        path: PathBuf,
    },
    /// Open one profile's document in `$EDITOR`.
    ///
    /// The uid is passed rather than a path because the document naming scheme
    /// belongs to the profile store, not to the interface.
    EditProfile {
        /// Profile uid.
        uid: String,
    },
}

impl Effect {
    /// A short verb, for tracing in the binary.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Startup => "startup",
            Self::Quit => "quit",
            Self::Refresh(_) => "refresh",
            Self::LoadProfiles => "load profiles",
            Self::SwitchProfile { .. } => "switch profile",
            Self::UpdateProfiles { .. } => "update profiles",
            Self::SetChain { .. } => "save chain",
            Self::PreviewConfig => "preview config",
            Self::ApplyConfig { .. } => "apply config",
            Self::PrepareConfig => "prepare config",
            Self::RollbackConfig => "roll back",
            Self::DeleteProfile { .. } => "delete profile",
            Self::RenameProfile { .. } => "rename profile",
            Self::NewProfile { .. } => "new profile",
            Self::DetectImportSources => "find import sources",
            Self::ImportProfiles { .. } => "import profiles",
            Self::SelectNode { .. } => "select node",
            Self::ClearNodePin { .. } => "clear pin",
            Self::TestNode { .. } => "test node",
            Self::TestGroup { .. } => "test group",
            Self::TestAllNodes => "test all nodes",
            Self::RunTest { .. } => "run test",
            Self::CancelTests => "cancel tests",
            Self::ClearTestResults => "clear results",
            Self::CloseConnection { .. } => "close connection",
            Self::CloseAllConnections => "close all connections",
            Self::ToggleRule { .. } => "toggle rule",
            Self::UpdateRuleProviders { .. } => "update rule sets",
            Self::SetCoreLogLevel(_) => "set log level",
            Self::SetCoreMode { .. } => "set route mode",
            Self::StartCore => "start core",
            Self::StopCore => "stop core",
            Self::RestartCore => "restart core",
            Self::UpgradeCore => "install managed core",
            Self::UpdateGeo => "update geo databases",
            Self::FlushCaches => "flush caches",
            Self::SaveSettings { .. } => "save settings",
            Self::ExportLogs { .. } => "export logs",
            Self::OpenEditor { .. } => "open editor",
            Self::EditProfile { .. } => "edit profile",
        }
    }
}

// ---------------------------------------------------------------------------
// events coming back
// ---------------------------------------------------------------------------

/// One entry of a [`Preview`]'s change list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewChange {
    /// `+`, `-`, `~` or `=`.
    pub verb: char,
    /// Dotted path of what changed.
    pub path: String,
}

/// One validation finding in a [`Preview`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewFinding {
    /// `E`, `W` or `i`.
    pub tag: char,
    /// What is wrong.
    pub message: String,
    /// Where it is, when the checker knew.
    pub location: Option<String>,
    /// How to fix it.
    pub hint: Option<String>,
}

/// What regenerating the configuration would change.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Preview {
    /// One-line description of the generated document.
    pub summary: String,
    /// Structural changes against the previous generated document.
    pub changes: Vec<PreviewChange>,
    /// Validation findings.
    pub findings: Vec<PreviewFinding>,
    /// Non-fatal problems the pipeline reported.
    pub warnings: Vec<String>,
    /// Whether the document passed validation.
    pub applicable: bool,
    /// Whether the change list was cut short.
    pub truncated: bool,
}

impl Preview {
    /// Render the preview as scrolling text.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let mut out = vec![self.summary.clone(), String::new()];
        if self.findings.is_empty() {
            out.push("no validation findings".to_owned());
        } else {
            for finding in &self.findings {
                let location = finding
                    .location
                    .as_deref()
                    .map_or_else(String::new, |l| format!(" ({l})"));
                out.push(format!("{} {}{}", finding.tag, finding.message, location));
                if let Some(hint) = &finding.hint {
                    out.push(format!("    hint: {hint}"));
                }
            }
        }
        if !self.warnings.is_empty() {
            out.push(String::new());
            for warning in &self.warnings {
                out.push(format!("! {warning}"));
            }
        }
        out.push(String::new());
        if self.changes.is_empty() {
            out.push("no structural changes against the previous configuration".to_owned());
        } else {
            out.push(format!("{} change(s):", self.changes.len()));
            for change in &self.changes {
                out.push(format!("  {} {}", change.verb, change.path));
            }
            if self.truncated {
                out.push("  … the list was truncated".to_owned());
            }
        }
        out
    }
}

/// Fresh data from the binary.
///
/// Rows arrive already translated (see [`crate::row`]), so the machine never
/// has to reach into a nested core response, and a payload can be built in a
/// test without a core.
#[derive(Debug, Clone, PartialEq)]
pub enum Data {
    /// The profile store.
    Profiles(Vec<ProfileRow>),
    /// The proxy tree: group rows followed by their members.
    Nodes(Vec<NodeRow>),
    /// A snapshot of the connection table.
    Connections(Vec<ConnectionRow>),
    /// The running rule list.
    Rules(Vec<RuleRow>),
    /// The names of the rule providers the core knows.
    RuleProviders(Vec<String>),
    /// One log line.
    Log(LogRow),
    /// One traffic sample.
    Traffic(Traffic),
    /// One memory sample, in bytes.
    Memory(u64),
    /// What the supervisor reports about the core.
    Core(CoreStatus),
    /// Routing mode reported by the running core.
    CoreMode(String),
    /// The core's version string.
    Version(String),
    /// Settings read from disk.
    Settings(Box<Settings>),
    /// A preview of the generated configuration.
    Preview(Box<Preview>),
    /// Directories that look like an existing `clash-verge-rev` home.
    ImportSources(Vec<PathBuf>),
    /// One test finished, or started.
    TestResult {
        /// Which check it was.
        kind: TestKind,
        /// What it ran against.
        target: String,
        /// How it ended.
        result: TestResult,
    },
    /// A note from the binary that is worth showing but is not a failure.
    Notice(String),
}

/// Something the binary finished.
///
/// Each variant becomes a status message, so the user learns that a request
/// that takes a second or more actually completed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Done {
    /// The profile list was re-read.
    ProfilesLoaded,
    /// A profile became the base document.
    ProfileSwitched {
        /// Its name.
        name: String,
    },
    /// Subscriptions finished downloading.
    ProfilesUpdated {
        /// How many succeeded.
        updated: usize,
        /// How many failed.
        failed: usize,
    },
    /// The explicit chain was written.
    ChainSaved,
    /// A configuration reached the core.
    ConfigApplied {
        /// How it reached the core, when it did.
        reload: Option<cvt_core::ReloadOutcome>,
        /// How many structural changes the generation produced.
        changed: usize,
    },
    /// The previous configuration was restored.
    ConfigRolledBack {
        /// The snapshot that was put back.
        snapshot: PathBuf,
    },
    /// A profile and its document were deleted.
    ProfileDeleted {
        /// What it was called.
        name: String,
        /// Whether it was the active base profile.
        was_current: bool,
    },
    /// A profile was renamed.
    ProfileRenamed {
        /// The new name.
        name: String,
    },
    /// A profile was created.
    ProfileCreated {
        /// Its name.
        name: String,
        /// Its UID, if known.
        uid: Option<String>,
        /// Whether it was created from a URL.
        is_remote: bool,
    },
    /// Profiles were imported from another installation.
    ProfilesImported {
        /// How many arrived.
        count: usize,
    },
    /// A node was pinned.
    NodeSelected {
        /// Group name.
        group: String,
        /// Member name.
        member: String,
    },
    /// A group went back to automatic selection.
    NodeCleared {
        /// Group name.
        group: String,
    },
    /// A latency test batch finished.
    NodeTestsFinished {
        /// How many nodes were measured.
        tested: usize,
    },
    /// One connection was dropped.
    ConnectionClosed,
    /// Every connection was dropped.
    ConnectionsClosed {
        /// How many there were.
        count: usize,
    },
    /// A rule changed state.
    RuleToggled {
        /// Rule position.
        index: u32,
        /// Its new state.
        disabled: bool,
    },
    /// Rule sets finished downloading.
    RuleProvidersUpdated {
        /// How many were updated.
        count: usize,
    },
    /// The core started.
    CoreStarted {
        /// The new process id.
        pid: u32,
    },
    /// The core stopped.
    CoreStopped,
    /// The core restarted.
    CoreRestarted {
        /// The new process id.
        pid: u32,
    },
    /// The live routing mode changed.
    CoreModeChanged {
        /// The mode now running.
        mode: String,
    },
    /// A newer core was installed.
    CoreUpgraded {
        /// The new version string.
        version: String,
    },
    /// The geo databases were refreshed.
    GeoUpdated,
    /// The fake-IP and DNS caches were cleared.
    CachesFlushed,
    /// The settings reached the disk.
    SettingsSaved,
    /// Log lines were written out.
    LogsExported {
        /// Where they went.
        path: PathBuf,
    },
    /// A file was handed to `$EDITOR`.
    EditorOpened {
        /// What was opened.
        target: String,
    },
}

/// Everything that can happen to the application.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A key was pressed.
    Key(KeyEvent),
    /// A mouse button or wheel moved over the interface.
    Mouse(MouseEvent),
    /// The frame timer fired.
    Tick,
    /// The terminal changed size.
    Resize(u16, u16),
    /// The binary has data.
    Data(Data),
    /// The binary finished something.
    Done(Done),
    /// Something failed, with a message worth reading.
    Failed(String),
}

// ---------------------------------------------------------------------------
// overlays and status
// ---------------------------------------------------------------------------

/// How bad a status message is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    /// Neutral information.
    Info,
    /// Something worked.
    Success,
    /// Something was refused, or worked only partly.
    Warning,
    /// Something failed.
    Error,
}

/// One line of feedback for the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// How bad it is.
    pub kind: StatusKind,
    /// What to say.
    pub text: String,
    /// When it was raised.
    pub at: Instant,
}

impl Status {
    /// A message raised now.
    #[must_use]
    pub fn new(kind: StatusKind, text: impl Into<String>) -> Self {
        Self {
            kind,
            text: text.into(),
            at: Instant::now(),
        }
    }

    /// Whether this message should be hidden at `now`.
    ///
    /// Transient messages carry a timestamp so that a burst of them cannot
    /// pile up: the newest replaces the previous one and goes away by itself.
    /// The complete message is retained separately after the footer expires.
    #[must_use]
    pub fn is_expired_at(&self, now: Instant) -> bool {
        now.duration_since(self.at) >= STATUS_TTL
    }
}

/// What a [`Overlay::Prompt`] is collecting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    /// A filter for the current list, applied as it is typed.
    Search,
    /// A new name for the highlighted profile.
    Rename,
    /// A subscription URL.
    Url,
    /// A name for a new, blank profile.
    Name,
    /// A new value for the highlighted setting.
    Text,
}

impl PromptKind {
    /// The default label, used when nothing more specific is known.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Search => "filter",
            Self::Rename => "rename profile",
            Self::Url => "subscription URL",
            Self::Name => "profile name",
            Self::Text => "value",
        }
    }
}

/// A modal layer that takes keys before the key map sees them.
///
/// This is what makes `q` type a `q` into a prompt instead of quitting: the
/// dispatcher never runs while an overlay is open.
#[derive(Debug, Clone, PartialEq)]
pub enum Overlay {
    /// A single-line text input.
    Prompt {
        /// What is being asked for.
        label: String,
        /// What the answer is used for.
        kind: PromptKind,
        /// The text so far.
        value: String,
        /// Caret position, counted in characters rather than bytes.
        cursor: usize,
    },
    /// A yes/no question about a destructive action.
    Confirm {
        /// What the user is about to do.
        question: String,
        /// The action to run on an explicit yes.
        action: Action,
    },
    /// A list of choices.
    Picker {
        /// What is being chosen.
        title: String,
        /// The choices.
        items: Vec<String>,
        /// Which one is highlighted.
        selected: usize,
    },
    /// Scrolling text, used for the configuration preview.
    Preview {
        /// What is being shown.
        title: String,
        /// The text, one entry per line.
        lines: Vec<String>,
        /// First line on screen.
        scroll: usize,
    },
    /// A modal displaying a full status or error message.
    Message {
        /// Popup title.
        title: String,
        /// Full message text.
        text: String,
        /// Message severity.
        kind: StatusKind,
        /// First rendered row on screen.
        scroll: usize,
    },
}

impl Overlay {
    /// A one-line description, for the frame title.
    #[must_use]
    pub fn title(&self) -> String {
        match self {
            Self::Prompt { label, .. } => label.clone(),
            Self::Confirm { question, .. } => question.clone(),
            Self::Picker { title, .. }
            | Self::Preview { title, .. }
            | Self::Message { title, .. } => title.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// settings rows
// ---------------------------------------------------------------------------

/// How a setting's value changes when it is toggled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingKind {
    /// A switch that flips between yes and no.
    Bool,
    /// A number that steps through a fixed set of sensible values.
    ///
    /// Free-form numbers would need validation feedback for every keystroke;
    /// a list of values the underlying validator accepts cannot be wrong.
    Number {
        /// The values, ascending.
        presets: &'static [i64],
    },
    /// One of a fixed set of strings.
    Choice {
        /// The values, in cycle order.
        options: &'static [&'static str],
    },
    /// Free text, edited in a prompt.
    Text,
}

/// One editable row of the settings screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingRow {
    /// Dotted key, e.g. `ui.refresh_ms`.
    pub key: &'static str,
    /// Human-readable name.
    pub label: &'static str,
    /// The current value, rendered for the value column.
    pub value: String,
    /// The same value in the form the prompt should be seeded with.
    ///
    /// Separate from `value` because they are not the same string: a setting
    /// that is unset shows `(none)`, and one that falls back to the base
    /// profile shows `from the base profile`. Seeding the prompt with the
    /// *rendering* turned the two keystrokes that open and close it — `s`, then
    /// Enter — into a silent edit that wrote `from the base profile` into
    /// `core.external_controller`, which the settings accepted and the
    /// generator then refused every configuration for.
    ///
    /// Empty means "unset", and the setters read it that way.
    pub editable: String,
    /// What changing it does.
    pub help: &'static str,
    /// How it changes.
    pub kind: SettingKind,
}

impl Filterable for SettingRow {
    fn matches_filter(&self, needle: &str) -> bool {
        contains_ignore_case(self.key, needle)
            || contains_ignore_case(self.label, needle)
            || contains_ignore_case(&self.value, needle)
    }
}

const REFRESH_PRESETS: &[i64] = &[250, 500, 1000, 2000, 5000];
const TIMEOUT_PRESETS: &[i64] = &[1000, 2000, 5000, 10_000, 30_000];
const CONCURRENCY_PRESETS: &[i64] = &[1, 2, 4, 8, 16, 32, 64];

/// Log sizes a person might actually pick, in bytes.
///
/// Zero is first because it is the off switch, and the rest step by a factor of
/// four: a log that is being rotated too often and one that is never rotated are
/// both obvious from the file, unlike a wrong refresh interval.
const LOG_SIZE_PRESETS: &[i64] = &[0, 64 * 1024, 256 * 1024, 1024 * 1024, 8 * 1024 * 1024];

/// How many rotated copies to keep. The validator refuses more than 64.
const LOG_KEEP_PRESETS: &[i64] = &[1, 2, 4, 8, 16, 32, 64];

/// How long a rotated copy may sit before it is deleted.
const LOG_DAYS_PRESETS: &[i64] = &[0, 1, 3, 7, 14, 30, 90];
const LOG_LEVELS: &[&str] = &["silent", "error", "warning", "info", "debug"];
const LANGUAGES: &[&str] = &["en", "zh-CN"];

/// Every setting the screen offers, with its current value.
#[must_use]
pub fn setting_rows(settings: &Settings) -> Vec<SettingRow> {
    let yes_no = |value: bool| if value { "yes" } else { "no" }.to_owned();
    let mut rows = vec![
        SettingRow {
            key: "core.binary",
            label: "core binary",
            value: settings
                .core
                .binary
                .as_ref()
                .map_or_else(|| "discovered".to_owned(), |p| p.display().to_string()),
            editable: settings
                .core
                .binary
                .as_ref()
                .map_or_else(String::new, |p| p.display().to_string()),
            help: "an explicit path to the mihomo binary; empty means search for one",
            kind: SettingKind::Text,
        },
        SettingRow {
            key: "core.external_controller",
            label: "controller address",
            value: settings
                .core
                .external_controller
                .clone()
                .unwrap_or_else(|| "from the base profile".to_owned()),
            editable: settings
                .core
                .external_controller
                .clone()
                .unwrap_or_default(),
            help: "where the core API listens; overrides the base profile's address",
            kind: SettingKind::Text,
        },
        SettingRow {
            key: "core.secret",
            label: "controller secret",
            value: settings
                .core
                .secret
                .as_ref()
                .map_or_else(|| "none".to_owned(), |_| "set".to_owned()),
            editable: settings.core.secret.clone().unwrap_or_default(),
            help: "the bearer token the core requires; stored in plain text, like the \
                   rest of this file",
            kind: SettingKind::Text,
        },
        SettingRow {
            key: "core.auto_start",
            label: "start the core on launch",
            value: yes_no(settings.core.auto_start),
            editable: yes_no(settings.core.auto_start),
            help: "launch the core as soon as the application starts",
            kind: SettingKind::Bool,
        },
        SettingRow {
            key: "core.rollback_on_failure",
            label: "roll back a bad configuration",
            value: yes_no(settings.core.rollback_on_failure),
            editable: yes_no(settings.core.rollback_on_failure),
            help: "restore the last snapshot when the core refuses the new one",
            kind: SettingKind::Bool,
        },
        SettingRow {
            key: "core.use_managed",
            label: "use managed core",
            value: yes_no(settings.core.use_managed),
            editable: yes_no(settings.core.use_managed),
            help: "use TUI-managed core in ~/.config/clash-verge-tui/core/mihomo instead of local/system core",
            kind: SettingKind::Bool,
        },
        SettingRow {
            key: "ui.language",
            label: "language",
            value: settings.ui.language.native_name().to_owned(),
            editable: settings.ui.language.code().to_owned(),
            help: "interface language; changes immediately and is saved with settings",
            kind: SettingKind::Choice { options: LANGUAGES },
        },
        SettingRow {
            key: "ui.refresh_ms",
            label: "refresh interval",
            value: format!("{} ms", settings.ui.refresh_ms),
            editable: settings.ui.refresh_ms.to_string(),
            help: "how often the interface redraws",
            kind: SettingKind::Number {
                presets: REFRESH_PRESETS,
            },
        },
        SettingRow {
            key: "ui.log_level",
            label: "log level",
            value: settings.ui.log_level.as_str().to_owned(),
            editable: settings.ui.log_level.as_str().to_owned(),
            help: "the minimum level shown, and requested from the core",
            kind: SettingKind::Choice {
                options: LOG_LEVELS,
            },
        },
        SettingRow {
            key: "ui.show_footer",
            label: "show the key hints",
            value: yes_no(settings.ui.show_footer),
            editable: yes_no(settings.ui.show_footer),
            help: "the footer line naming the keys that apply here",
            kind: SettingKind::Bool,
        },
        SettingRow {
            key: "ui.color",
            label: "colour",
            value: yes_no(settings.ui.color),
            editable: yes_no(settings.ui.color),
            help: "turn colours off for a monochrome terminal",
            kind: SettingKind::Bool,
        },
        SettingRow {
            key: "test.url",
            label: "latency test URL",
            value: settings.test.url.clone(),
            editable: settings.test.url.clone(),
            help: "must answer 204 without a body, so setup time is measured",
            kind: SettingKind::Text,
        },
        SettingRow {
            key: "test.timeout_ms",
            label: "test timeout",
            value: format!("{} ms", settings.test.timeout_ms),
            editable: settings.test.timeout_ms.to_string(),
            help: "how long a single measurement may take",
            kind: SettingKind::Number {
                presets: TIMEOUT_PRESETS,
            },
        },
        SettingRow {
            key: "logs.max_size_bytes",
            label: "rotate a log after",
            value: crate::state::human_bytes(settings.logs.max_size_bytes),
            editable: settings.logs.max_size_bytes.to_string(),
            help: "the core's log and this program's are rotated when the core is \
                   started; zero turns rotation off",
            kind: SettingKind::Number {
                presets: LOG_SIZE_PRESETS,
            },
        },
        SettingRow {
            key: "logs.keep",
            label: "rotated copies kept",
            value: settings.logs.keep.to_string(),
            editable: settings.logs.keep.to_string(),
            help: "how many older copies to keep per log, oldest dropped first",
            kind: SettingKind::Number {
                presets: LOG_KEEP_PRESETS,
            },
        },
        SettingRow {
            key: "logs.keep_days",
            label: "delete copies after",
            value: format!("{} days", settings.logs.keep_days),
            editable: settings.logs.keep_days.to_string(),
            help: "rotated copies older than this are removed; zero keeps them all",
            kind: SettingKind::Number {
                presets: LOG_DAYS_PRESETS,
            },
        },
        SettingRow {
            key: "test.concurrency",
            label: "parallel tests",
            value: settings.test.concurrency.to_string(),
            editable: settings.test.concurrency.to_string(),
            help: "how many nodes are measured at once",
            kind: SettingKind::Number {
                presets: CONCURRENCY_PRESETS,
            },
        },
        SettingRow {
            key: "test.expected_status",
            label: "expected status",
            value: settings.test.expected_status.clone(),
            editable: settings.test.expected_status.clone(),
            help: "accept this status expression; `*` accepts anything",
            kind: SettingKind::Text,
        },
        SettingRow {
            key: "stream.traffic",
            label: "subscribe to traffic",
            value: yes_no(settings.stream.traffic),
            editable: yes_no(settings.stream.traffic),
            help: "the dashboard's throughput gauges",
            kind: SettingKind::Bool,
        },
        SettingRow {
            key: "stream.memory",
            label: "subscribe to memory",
            value: yes_no(settings.stream.memory),
            editable: yes_no(settings.stream.memory),
            help: "the core's resident memory reading",
            kind: SettingKind::Bool,
        },
        SettingRow {
            key: "stream.logs",
            label: "subscribe to logs",
            value: yes_no(settings.stream.logs),
            editable: yes_no(settings.stream.logs),
            help: "the live log stream",
            kind: SettingKind::Bool,
        },
        SettingRow {
            key: "stream.connections",
            label: "subscribe to connections",
            value: yes_no(settings.stream.connections),
            editable: yes_no(settings.stream.connections),
            help: "the live connection table",
            kind: SettingKind::Bool,
        },
        SettingRow {
            key: "update.update_on_start",
            label: "update profiles on launch",
            value: yes_no(settings.update.update_on_start),
            editable: yes_no(settings.update.update_on_start),
            help: "download every subscription that is due at start-up",
            kind: SettingKind::Bool,
        },
        SettingRow {
            key: "update.close_connections_on_apply",
            label: "close connections when applying",
            value: yes_no(settings.update.close_connections_on_apply),
            editable: yes_no(settings.update.close_connections_on_apply),
            help: "so nothing keeps using a node the new configuration dropped",
            kind: SettingKind::Bool,
        },
        SettingRow {
            key: "update.prefer_hot_reload",
            label: "prefer hot reload",
            value: yes_no(settings.update.prefer_hot_reload),
            editable: yes_no(settings.update.prefer_hot_reload),
            help: "hand the configuration to the API instead of restarting",
            kind: SettingKind::Bool,
        },
    ];
    if settings.ui.language == Language::Chinese {
        for row in &mut rows {
            if let Some((label, help)) = crate::i18n::setting_text(settings.ui.language, row.key) {
                row.label = label;
                row.help = help;
            }
            if row.kind == SettingKind::Bool
                || row.key == "core.secret"
                || (row.key == "core.binary" && settings.core.binary.is_none())
                || (row.key == "core.external_controller"
                    && settings.core.external_controller.is_none())
            {
                row.value = crate::i18n::text(settings.ui.language, &row.value).to_owned();
            }
        }
    }
    rows
}

/// Move a numeric setting to the next preset, or to the nearest one when the
/// current value is not a preset.
fn step_number(current: i64, presets: &[i64], forward: bool) -> i64 {
    if presets.is_empty() {
        return current;
    }
    let position = presets.iter().position(|p| *p == current);
    let Some(position) = position else {
        return presets
            .iter()
            .find(|p| **p >= current)
            .copied()
            .unwrap_or_else(|| presets[presets.len() - 1]);
    };
    let next = if forward {
        (position + 1) % presets.len()
    } else {
        (position + presets.len() - 1) % presets.len()
    };
    presets[next]
}

/// Advance a string setting to the next option.
fn step_choice(current: &str, options: &[&str]) -> Option<String> {
    if options.is_empty() {
        return None;
    }
    let position = options.iter().position(|o| *o == current).unwrap_or(0);
    Some(options[(position + 1) % options.len()].to_owned())
}

/// Change one setting by key, the way [`Action::ToggleSetting`] does.
///
/// Returns `false` for a key that is not a switch, a number or a choice — the
/// caller opens a prompt for those instead.
fn cycle_setting(settings: &mut Settings, key: &str, forward: bool) -> bool {
    match key {
        "ui.language" => {
            settings.ui.language = settings.ui.language.next();
            true
        }
        "core.auto_start" => {
            settings.core.auto_start = !settings.core.auto_start;
            true
        }
        "core.rollback_on_failure" => {
            settings.core.rollback_on_failure = !settings.core.rollback_on_failure;
            true
        }
        "core.use_managed" => {
            settings.core.use_managed = !settings.core.use_managed;
            true
        }
        "ui.refresh_ms" => {
            let next = step_number(
                i64::try_from(settings.ui.refresh_ms).unwrap_or(i64::MAX),
                REFRESH_PRESETS,
                forward,
            );
            settings.ui.refresh_ms = u64::try_from(next).unwrap_or(settings.ui.refresh_ms);
            true
        }
        "ui.log_level" => {
            let options: Vec<&str> = LogLevel::all().iter().map(|l| l.as_str()).collect();
            let Some(next) = step_choice(settings.ui.log_level.as_str(), &options) else {
                return false;
            };
            settings.ui.log_level = LogLevel::parse(&next).unwrap_or(settings.ui.log_level);
            true
        }
        "ui.show_footer" => {
            settings.ui.show_footer = !settings.ui.show_footer;
            true
        }
        "ui.color" => {
            settings.ui.color = !settings.ui.color;
            true
        }
        "test.timeout_ms" => {
            let next = step_number(
                i64::from(settings.test.timeout_ms),
                TIMEOUT_PRESETS,
                forward,
            );
            settings.test.timeout_ms = u32::try_from(next).unwrap_or(settings.test.timeout_ms);
            true
        }
        "logs.max_size_bytes" => {
            let next = step_number(
                i64::try_from(settings.logs.max_size_bytes).unwrap_or(i64::MAX),
                LOG_SIZE_PRESETS,
                forward,
            );
            settings.logs.max_size_bytes = u64::try_from(next).unwrap_or(0);
            true
        }
        "logs.keep" => {
            let next = step_number(
                i64::try_from(settings.logs.keep).unwrap_or(i64::MAX),
                LOG_KEEP_PRESETS,
                forward,
            );
            settings.logs.keep = usize::try_from(next).unwrap_or(1);
            true
        }
        "logs.keep_days" => {
            let next = step_number(
                i64::try_from(settings.logs.keep_days).unwrap_or(i64::MAX),
                LOG_DAYS_PRESETS,
                forward,
            );
            settings.logs.keep_days = u64::try_from(next).unwrap_or(0);
            true
        }
        "test.concurrency" => {
            let next = step_number(
                i64::try_from(settings.test.concurrency).unwrap_or(i64::MAX),
                CONCURRENCY_PRESETS,
                forward,
            );
            settings.test.concurrency = usize::try_from(next).unwrap_or(settings.test.concurrency);
            true
        }
        "stream.traffic" => {
            settings.stream.traffic = !settings.stream.traffic;
            true
        }
        "stream.memory" => {
            settings.stream.memory = !settings.stream.memory;
            true
        }
        "stream.logs" => {
            settings.stream.logs = !settings.stream.logs;
            true
        }
        "stream.connections" => {
            settings.stream.connections = !settings.stream.connections;
            true
        }
        "update.update_on_start" => {
            settings.update.update_on_start = !settings.update.update_on_start;
            true
        }
        "update.close_connections_on_apply" => {
            settings.update.close_connections_on_apply =
                !settings.update.close_connections_on_apply;
            true
        }
        "update.prefer_hot_reload" => {
            settings.update.prefer_hot_reload = !settings.update.prefer_hot_reload;
            true
        }
        _ => false,
    }
}

/// Write a typed-in value into a setting, and refuse one the settings refuse.
///
/// The doc used to say "validating here rather than on save means a mistyped URL
/// is refused while the prompt is still open" — true of the URL arms and not of
/// the numeric ones, which took any parseable number. `test.timeout_ms: 99999`
/// was accepted into memory, sat there until a save failed, and was read by
/// everything that consults the settings in between.
///
/// The wrapper makes the sentence true for every arm: the value is written,
/// checked against the same `validate` a save runs, and rolled back with the
/// reason if it does not hold.
fn set_setting_text(settings: &mut Settings, key: &str, text: &str) -> Result<(), String> {
    let before = settings.clone();
    let result = set_setting_text_inner(settings, key, text);
    if let Err(reason) = result {
        *settings = before;
        return Err(reason);
    }
    match settings.validate() {
        Ok(()) => Ok(()),
        Err(error) => {
            *settings = before;
            Err(error.to_string())
        }
    }
}

fn set_setting_text_inner(settings: &mut Settings, key: &str, text: &str) -> Result<(), String> {
    let text = text.trim();
    match key {
        "core.binary" => {
            settings.core.binary = if text.is_empty() {
                None
            } else {
                Some(PathBuf::from(text))
            };
            Ok(())
        }
        "core.external_controller" => {
            settings.core.external_controller = if text.is_empty() {
                None
            } else {
                Some(text.to_owned())
            };
            Ok(())
        }
        "core.secret" => {
            settings.core.secret = if text.is_empty() {
                None
            } else {
                Some(text.to_owned())
            };
            Ok(())
        }
        "test.url" => {
            if !text.starts_with("http://") && !text.starts_with("https://") {
                return Err("the test URL must start with http:// or https://".to_owned());
            }
            text.clone_into(&mut settings.test.url);
            Ok(())
        }
        "test.expected_status" => {
            if text.is_empty() {
                return Err(
                    "a status expression is required; use `*` to accept anything".to_owned(),
                );
            }
            text.clone_into(&mut settings.test.expected_status);
            Ok(())
        }
        "ui.refresh_ms" => {
            let value: u64 = text
                .parse()
                .map_err(|_| "expected a number of ms".to_owned())?;
            settings.ui.refresh_ms = value;
            Ok(())
        }
        "test.timeout_ms" => {
            let value: u32 = text
                .parse()
                .map_err(|_| "expected a number of ms".to_owned())?;
            settings.test.timeout_ms = value;
            Ok(())
        }
        "test.concurrency" => {
            let value: usize = text.parse().map_err(|_| "expected a number".to_owned())?;
            settings.test.concurrency = value;
            Ok(())
        }
        other => Err(format!("`{other}` is not a text setting")),
    }
}

// ---------------------------------------------------------------------------
// connection ordering
// ---------------------------------------------------------------------------

/// How the connection table is ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConnectionSort {
    /// As the core reported them.
    #[default]
    Natural,
    /// Largest total traffic first.
    Busiest,
    /// Open longest first.
    Oldest,
    /// Most recently opened first.
    Newest,
}

impl ConnectionSort {
    /// The next order in the cycle.
    #[must_use]
    pub fn next(self) -> Self {
        match self {
            Self::Natural => Self::Busiest,
            Self::Busiest => Self::Oldest,
            Self::Oldest => Self::Newest,
            Self::Newest => Self::Natural,
        }
    }

    /// A short label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Natural => "natural",
            Self::Busiest => "busiest",
            Self::Oldest => "oldest",
            Self::Newest => "newest",
        }
    }
}

// ---------------------------------------------------------------------------
// list access
// ---------------------------------------------------------------------------

/// The operations the dispatcher performs on whichever list is on screen.
///
/// Implemented for [`Table`] and for [`LogBuffer`]; the logs screen has a
/// filter but no cursor, so its cursor operations are no-ops. Keeping this as
/// one trait is what lets navigation, paging and search share a single code
/// path instead of a screen-shaped `match` in every handler.
trait Rows {
    /// Replace the filter, keeping the cursor on the same row where possible.
    fn set_filter(&mut self, needle: &str);
    /// The current filter.
    fn filter(&self) -> String;
    /// Discard the filter.
    fn clear_filter(&mut self) {
        self.set_filter("");
    }
    /// Move the cursor.
    fn move_by(&mut self, _delta: isize) {}
    /// Move the cursor by a screenful.
    fn page(&mut self, _direction: isize, _viewport: usize) {}
    /// Move the cursor to the first row.
    fn select_first(&mut self) {}
    /// Move the cursor to the last row.
    fn select_last(&mut self) {}
    /// Move the cursor to the next row matching the filter.
    fn next_match(&mut self) {}
    /// Scroll so the cursor is inside a viewport.
    fn scroll_into_view(&mut self, _height: usize) {}
    /// How many rows pass the filter.
    fn len(&self) -> usize;
    /// How many rows there are in total.
    fn total(&self) -> usize;
}

impl<T: Filterable> Rows for Table<T> {
    fn set_filter(&mut self, needle: &str) {
        Self::set_filter(self, needle);
    }

    fn filter(&self) -> String {
        Self::filter(self).to_owned()
    }

    fn move_by(&mut self, delta: isize) {
        Self::move_by(self, delta);
    }

    fn page(&mut self, direction: isize, viewport: usize) {
        Self::page(self, direction, viewport);
    }

    fn select_first(&mut self) {
        Self::select_first(self);
    }

    fn select_last(&mut self) {
        Self::select_last(self);
    }

    fn next_match(&mut self) {
        let needle = Self::filter(self).to_owned();
        if needle.is_empty() {
            Self::move_by(self, 1);
            return;
        }
        self.select_next_matching(|row| row.matches_filter(&needle));
    }

    fn scroll_into_view(&mut self, height: usize) {
        Self::scroll_into_view(self, height);
    }

    fn len(&self) -> usize {
        Self::len(self)
    }

    fn total(&self) -> usize {
        Self::total(self)
    }
}

impl Rows for LogBuffer {
    fn set_filter(&mut self, needle: &str) {
        Self::set_filter(self, needle);
    }

    fn filter(&self) -> String {
        Self::filter(self).to_owned()
    }

    fn len(&self) -> usize {
        self.filtered().len()
    }

    fn total(&self) -> usize {
        Self::len(self)
    }
}

// ---------------------------------------------------------------------------
// the application
// ---------------------------------------------------------------------------

/// All the interface state, and the rules for changing it.
#[derive(Debug)]
pub struct App {
    /// Application home; the only path prefix the interface needs.
    pub home: PathBuf,
    /// The palette, rebuilt when the colour setting changes.
    pub theme: Theme,
    /// The screen on show.
    pub screen: Screen,
    /// The key map, consulted for every key press.
    pub keymap: Keymap,
    /// The modal layer on top, which consumes keys first.
    pub overlay: Option<Overlay>,
    /// Profiles, as the store lists them.
    pub profiles: Table<ProfileRow>,
    /// Proxy rows, flattened to what is currently visible.
    pub nodes: Table<NodeRow>,
    /// The connection table.
    pub connections: Table<ConnectionRow>,
    /// The rule table, excluding disabled rules unless asked for.
    pub rules: Table<RuleRow>,
    /// The tests screen.
    pub tests: Table<TestRow>,
    /// The settings screen.
    pub settings_rows: Table<SettingRow>,
    /// Buffered log lines.
    pub logs: LogBuffer,
    /// Rolling traffic and memory gauges.
    pub metrics: Metrics,
    /// What the supervisor last reported.
    pub core: CoreStatus,
    /// Routing mode reported by Mihomo's live configuration.
    pub core_mode: Option<String>,
    /// The core's version, once the binary has read it.
    pub version: Option<String>,
    /// The settings, with any unsaved edits.
    pub settings: Settings,
    /// Whether the settings differ from what is on disk.
    pub settings_dirty: bool,
    /// The minimum log level shown and requested from the core.
    pub log_level: LogLevel,
    /// Terminal size, as of the last [`Event::Resize`].
    pub viewport: (u16, u16),
    /// The most recent configuration preview.
    pub preview: Option<Preview>,
    /// How the connection table is ordered.
    pub connection_sort: ConnectionSort,
    /// How the proxies list is ordered; a finished measurement batch reorders
    /// it fastest-first, because that is what the user just asked to know.
    pub node_sort: SortOrder,
    /// Whether disabled rules are listed.
    pub show_disabled_rules: bool,
    status: Option<Status>,
    last_status: Option<Status>,
    ticks: u64,
    quit: bool,
    chain: Vec<String>,
    all_nodes: Vec<NodeRow>,
    expanded: Vec<String>,
    all_rules: Vec<RuleRow>,
    rule_providers: Vec<String>,
    queue: VecDeque<usize>,
    in_flight: Option<usize>,
    frozen: Option<usize>,
    loaded: Vec<Screen>,
}

impl App {
    /// Language currently selected for the interface.
    #[must_use]
    pub fn language(&self) -> Language {
        self.settings.ui.language
    }

    /// Translate an interface-owned message, preserving unknown text.
    #[must_use]
    pub fn tr<'a>(&self, english: &'a str) -> &'a str {
        crate::i18n::text(self.language(), english)
    }

    /// Format a status message, translating dynamic patterns in the chosen language.
    #[must_use]
    pub fn format_status_text(&self, text: &str) -> String {
        crate::i18n::format_status(self.language(), text)
    }

    /// Translate a context-specific interface label by its stable identity.
    #[must_use]
    pub(crate) fn tr_key(&self, key: crate::i18n::TextKey) -> &'static str {
        crate::i18n::label(self.language(), key)
    }

    /// A fresh application showing the dashboard, with no data.
    ///
    /// The settings shown are loaded from the home directory, or the defaults
    /// if not present, and updated whenever [`Data::Settings`] arrives.
    #[must_use]
    pub fn new(home: PathBuf, theme: Theme) -> Self {
        let settings =
            cvt_core::Settings::load(&cvt_core::AppPaths::new(&home)).unwrap_or_default();
        let log_level = settings.ui.log_level;
        let settings_rows = Table::from_items(setting_rows(&settings));
        let mut app = Self {
            home,
            theme,
            screen: Screen::Home,
            keymap: Keymap::new(),
            overlay: None,
            profiles: Table::new(),
            nodes: Table::new(),
            connections: Table::new(),
            rules: Table::new(),
            tests: Table::new(),
            settings_rows,
            logs: LogBuffer::new(LOG_CAPACITY),
            metrics: Metrics::new(METRIC_SAMPLES),
            core: CoreStatus::Stopped,
            core_mode: None,
            version: None,
            settings,
            settings_dirty: false,
            log_level,
            viewport: (80, 24),
            preview: None,
            connection_sort: ConnectionSort::Natural,
            node_sort: SortOrder::Natural,
            show_disabled_rules: false,
            status: None,
            last_status: None,
            ticks: 0,
            quit: false,
            chain: Vec::new(),
            all_nodes: Vec::new(),
            expanded: Vec::new(),
            all_rules: Vec::new(),
            rule_providers: Vec::new(),
            queue: VecDeque::new(),
            in_flight: None,
            frozen: None,
            loaded: Vec::new(),
        };
        app.rebuild_tests();
        app
    }

    // -- observable state ---------------------------------------------------

    /// Whether the application should stop.
    #[must_use]
    pub fn is_quit(&self) -> bool {
        self.quit
    }

    /// The status message to show, if it has not expired.
    #[must_use]
    pub fn current_status(&self) -> Option<&Status> {
        self.status
            .as_ref()
            .filter(|s| !s.is_expired_at(Instant::now()))
    }

    /// The most recent status message shown, even if expired.
    #[must_use]
    pub fn last_status(&self) -> Option<&Status> {
        self.last_status.as_ref()
    }

    /// The explicit patch chain, in application order.
    #[must_use]
    pub fn chain(&self) -> &[String] {
        &self.chain
    }

    /// Whether a group is expanded on the proxies screen.
    #[must_use]
    pub fn is_expanded(&self, group: &str) -> bool {
        self.expanded.iter().any(|g| g == group)
    }

    /// The names of the rule providers the core reported.
    #[must_use]
    pub fn rule_providers(&self) -> &[String] {
        &self.rule_providers
    }

    /// How many tests are queued or in flight.
    ///
    /// A test that has been asked for counts until it answers, so that
    /// cancelling works while the first one of a batch is still running.
    #[must_use]
    pub fn queued_tests(&self) -> usize {
        self.queue.len() + usize::from(self.in_flight.is_some())
    }

    /// How many rows the current screen can show at once.
    #[must_use]
    pub fn visible_rows(&self) -> usize {
        crate::ui::table_rows_area(self).map_or(1, |area| usize::from(area.height).max(1))
    }

    /// How many rules are hidden because they are disabled.
    #[must_use]
    pub fn hidden_rules(&self) -> usize {
        self.all_rules.len().saturating_sub(self.rules.total())
    }

    /// The filter in force on the screen being shown, if any.
    #[must_use]
    pub fn active_filter(&self) -> String {
        self.active_rows().map_or_else(String::new, Rows::filter)
    }

    /// The log lines the screen should show, and how many newer ones it hides.
    ///
    /// Stopping the follow does not discard anything: the window simply stops
    /// moving, and the count of what has arrived since is what the title
    /// reports, so the user can see that the stream is alive without it
    /// dragging the view away from the line being read.
    #[must_use]
    pub fn log_window(&self, height: usize) -> (Vec<&LogRow>, usize) {
        let lines = self.logs.filtered();
        let end = self
            .frozen
            .map_or(lines.len(), |frozen| frozen.min(lines.len()));
        let start = end.saturating_sub(height);
        (lines[start..end].to_vec(), lines.len() - end)
    }

    /// The path the logs would be exported to.
    #[must_use]
    pub fn log_export_path(&self) -> PathBuf {
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        self.home.join("logs").join(format!("cvt-logs-{stamp}.log"))
    }

    /// The generated runtime configuration's path.
    ///
    /// Mirrors `cvt_core::AppPaths::runtime_config`; the home is the one thing
    /// the interface is told about, and a path is all this effect needs.
    #[must_use]
    pub fn runtime_config_path(&self) -> PathBuf {
        self.home.join("runtime").join("config.yaml")
    }

    // -- the entry point ----------------------------------------------------

    /// Handle one event, and return what the binary should do about it.
    pub fn on_event(&mut self, event: Event) -> Vec<Effect> {
        match event {
            Event::Key(key) => self.on_key(key),
            Event::Mouse(mouse) => self.on_mouse(mouse),
            Event::Tick => self.on_tick(),
            Event::Resize(width, height) => {
                self.viewport = (width, height);
                self.sync_scroll();
                Vec::new()
            }
            Event::Data(data) => self.on_data(data),
            Event::Done(done) => self.on_done(done),
            Event::Failed(message) => {
                // A failure can be the answer to anything, including a queued
                // test, so nothing may stay marked as running.
                self.abandon_tests();
                self.set_status(StatusKind::Error, message);
                Vec::new()
            }
        }
    }

    /// Handle a frame tick.
    ///
    /// Expires transient messages and, while the core is up, asks for the
    /// current screen to be re-read now and then — the streams cover the live
    /// panes, but nothing pushes a changed rule list or a new process id.
    pub fn on_tick(&mut self) -> Vec<Effect> {
        self.ticks = self.ticks.wrapping_add(1);
        self.expire_status();
        if self.core.is_running() && self.ticks.is_multiple_of(POLL_EVERY) {
            return Self::refresh_effects(self.screen);
        }
        Vec::new()
    }

    // -- key routing --------------------------------------------------------

    fn on_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        // An overlay is modal: while one is open the key map is not consulted
        // at all, which is what stops `q` from quitting out of a prompt.
        if self.overlay.is_some() {
            return self.on_overlay_key(key);
        }
        match self.keymap.resolve(self.screen, key) {
            Some(action) => self.dispatch(action, false),
            None => Vec::new(),
        }
    }

    fn on_mouse(&mut self, mouse: MouseEvent) -> Vec<Effect> {
        if self.overlay.is_some() {
            return self.on_overlay_mouse(mouse);
        }
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if mouse.row == self.viewport.1.saturating_sub(1) {
                    return self.show_last_message();
                }
                if let Some(screen) = crate::ui::tab_at(self, mouse.column, mouse.row) {
                    return self.goto(screen);
                }
                let Some(area) = crate::ui::table_rows_area(self) else {
                    return Vec::new();
                };
                if !area.contains(ratatui::layout::Position::new(mouse.column, mouse.row)) {
                    return Vec::new();
                }
                let index =
                    self.active_table_offset() + usize::from(mouse.row.saturating_sub(area.y));
                if self.active_rows().is_some_and(|rows| index < rows.len()) {
                    self.select_table_row(index);
                }
                Vec::new()
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                if mouse.row > 0 && mouse.row < self.viewport.1.saturating_sub(1) =>
            {
                let delta = if matches!(mouse.kind, MouseEventKind::ScrollUp) {
                    -3
                } else {
                    3
                };
                if self.screen == Screen::Logs {
                    self.scroll_logs(delta);
                    Vec::new()
                } else {
                    self.move_cursor(delta)
                }
            }
            _ => Vec::new(),
        }
    }

    fn on_overlay_mouse(&mut self, mouse: MouseEvent) -> Vec<Effect> {
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            let Some(overlay) = self.overlay.clone() else {
                return Vec::new();
            };
            match overlay {
                Overlay::Prompt { .. } => {
                    if let Some(accept) = crate::ui::prompt_choice_at(self, mouse.column, mouse.row)
                    {
                        let key = if accept { KeyCode::Enter } else { KeyCode::Esc };
                        return self.on_overlay_key(KeyEvent::new(key, KeyModifiers::NONE));
                    }
                }
                Overlay::Confirm { question, .. } => {
                    if let Some(accept) =
                        crate::ui::confirm_choice_at(self, &question, mouse.column, mouse.row)
                    {
                        let key = if accept { KeyCode::Enter } else { KeyCode::Esc };
                        return self.on_overlay_key(KeyEvent::new(key, KeyModifiers::NONE));
                    }
                }
                Overlay::Picker {
                    title,
                    items,
                    selected,
                } => {
                    if let Some(index) = crate::ui::picker_item_at(
                        self.viewport,
                        items.len(),
                        selected,
                        mouse.column,
                        mouse.row,
                    ) {
                        self.overlay = None;
                        return self.choose(&title, &items, index);
                    }
                }
                Overlay::Preview { .. } | Overlay::Message { .. } => {
                    self.overlay = None;
                }
            }
            return Vec::new();
        }
        let Some(overlay) = self.overlay.as_mut() else {
            return Vec::new();
        };
        match (overlay, mouse.kind) {
            (
                Overlay::Message { scroll, .. } | Overlay::Preview { scroll, .. },
                MouseEventKind::ScrollUp,
            ) => {
                *scroll = scroll.saturating_sub(3);
            }
            (Overlay::Message { text, scroll, .. }, MouseEventKind::ScrollDown) => {
                let last = crate::ui::message_scroll_limit(self.viewport, text);
                *scroll = scroll.saturating_add(3).min(last);
            }
            (Overlay::Picker { selected, .. }, MouseEventKind::ScrollUp) => {
                *selected = selected.saturating_sub(3);
            }
            (
                Overlay::Picker {
                    items, selected, ..
                },
                MouseEventKind::ScrollDown,
            ) => {
                *selected = selected
                    .saturating_add(3)
                    .min(items.len().saturating_sub(1));
            }
            (Overlay::Preview { lines, scroll, .. }, MouseEventKind::ScrollDown) => {
                *scroll = scroll.saturating_add(3).min(lines.len().saturating_sub(1));
            }
            _ => {}
        }
        Vec::new()
    }

    fn on_overlay_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(overlay) = self.overlay.take() else {
            return Vec::new();
        };
        match overlay {
            Overlay::Prompt {
                kind,
                value,
                cursor,
                ..
            } => self.on_prompt_key(kind, value, cursor, key),
            Overlay::Confirm { question, action } => {
                if matches!(key.code, KeyCode::Char('y' | 'Y') | KeyCode::Enter) {
                    return self.dispatch(action, true);
                }
                if matches!(key.code, KeyCode::Char('n' | 'N') | KeyCode::Esc) {
                    self.set_status(StatusKind::Info, "cancelled");
                    return Vec::new();
                }
                // Anything else is swallowed rather than treated as a no.
                self.overlay = Some(Overlay::Confirm { question, action });
                Vec::new()
            }
            Overlay::Picker {
                title,
                items,
                selected,
            } => {
                let last = items.len().saturating_sub(1);
                match key.code {
                    KeyCode::Esc => {
                        self.set_status(StatusKind::Info, "cancelled");
                    }
                    KeyCode::Enter => return self.choose(&title, &items, selected),
                    KeyCode::Down | KeyCode::Char('j') => {
                        self.overlay = Some(Overlay::Picker {
                            title,
                            items,
                            selected: selected.saturating_add(1).min(last),
                        });
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        self.overlay = Some(Overlay::Picker {
                            title,
                            items,
                            selected: selected.saturating_sub(1),
                        });
                    }
                    KeyCode::Home | KeyCode::Char('g') => {
                        self.overlay = Some(Overlay::Picker {
                            title,
                            items,
                            selected: 0,
                        });
                    }
                    KeyCode::End | KeyCode::Char('G') => {
                        self.overlay = Some(Overlay::Picker {
                            title,
                            items,
                            selected: last,
                        });
                    }
                    _ => {
                        self.overlay = Some(Overlay::Picker {
                            title,
                            items,
                            selected,
                        });
                    }
                }
                Vec::new()
            }
            Overlay::Preview {
                title,
                lines,
                scroll,
            } => {
                let last = lines.len().saturating_sub(1);
                let page = self.visible_rows().max(1);
                let next = match key.code {
                    KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => None,
                    KeyCode::Down | KeyCode::Char('j') => Some(scroll.saturating_add(1).min(last)),
                    KeyCode::Up | KeyCode::Char('k') => Some(scroll.saturating_sub(1)),
                    KeyCode::PageDown | KeyCode::Char(' ') => Some(
                        scroll
                            .saturating_add(page)
                            .min(last.saturating_sub(page.saturating_sub(1))),
                    ),
                    KeyCode::PageUp => Some(scroll.saturating_sub(page)),
                    KeyCode::Home | KeyCode::Char('g') => Some(0),
                    KeyCode::End | KeyCode::Char('G') => Some(last.saturating_sub(page - 1)),
                    _ => Some(scroll),
                };
                match next {
                    Some(scroll) => {
                        self.overlay = Some(Overlay::Preview {
                            title,
                            lines,
                            scroll,
                        });
                    }
                    None => self.set_status(StatusKind::Info, "preview closed"),
                }
                Vec::new()
            }
            Overlay::Message {
                title,
                text,
                kind,
                scroll,
            } => {
                let limit = crate::ui::message_scroll_limit(self.viewport, &text);
                let next = match key.code {
                    KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => return Vec::new(),
                    KeyCode::Down | KeyCode::Char('j') => scroll.saturating_add(1).min(limit),
                    KeyCode::Up | KeyCode::Char('k') => scroll.saturating_sub(1),
                    KeyCode::PageDown => scroll.saturating_add(self.visible_rows()).min(limit),
                    KeyCode::PageUp => scroll.saturating_sub(self.visible_rows()),
                    KeyCode::Home => 0,
                    KeyCode::End => limit,
                    _ => scroll,
                };
                self.overlay = Some(Overlay::Message {
                    title,
                    text,
                    kind,
                    scroll: next,
                });
                Vec::new()
            }
        }
    }

    fn on_prompt_key(
        &mut self,
        kind: PromptKind,
        mut value: String,
        mut cursor: usize,
        key: KeyEvent,
    ) -> Vec<Effect> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                if kind == PromptKind::Search {
                    self.set_filter("");
                }
                self.set_status(StatusKind::Info, "cancelled");
                return Vec::new();
            }
            KeyCode::Enter => return self.commit_prompt(kind, &value),
            KeyCode::Backspace => {
                if cursor > 0 {
                    let from = char_to_byte(&value, cursor - 1);
                    let to = char_to_byte(&value, cursor);
                    value.replace_range(from..to, "");
                    cursor -= 1;
                }
            }
            KeyCode::Delete => {
                if cursor < value.chars().count() {
                    let from = char_to_byte(&value, cursor);
                    let to = char_to_byte(&value, cursor + 1);
                    value.replace_range(from..to, "");
                }
            }
            KeyCode::Left => cursor = cursor.saturating_sub(1),
            KeyCode::Right => {
                cursor = cursor.saturating_add(1).min(value.chars().count());
            }
            KeyCode::Home => cursor = 0,
            KeyCode::End => cursor = value.chars().count(),
            KeyCode::Char('u') if ctrl => {
                value.clear();
                cursor = 0;
            }
            KeyCode::Char(c) if !ctrl => {
                let at = char_to_byte(&value, cursor);
                value.insert(at, c);
                cursor += 1;
            }
            _ => {}
        }
        if kind == PromptKind::Search {
            self.set_filter(&value);
        }
        self.overlay = Some(Overlay::Prompt {
            label: self.prompt_label(kind),
            kind,
            value,
            cursor,
        });
        Vec::new()
    }

    fn prompt_label(&self, kind: PromptKind) -> String {
        if kind != PromptKind::Text {
            return kind.label().to_owned();
        }
        self.settings_rows
            .selected_item()
            .map_or_else(|| kind.label().to_owned(), |row| row.label.to_owned())
    }

    /// Commit a prompt.
    fn commit_prompt(&mut self, kind: PromptKind, value: &str) -> Vec<Effect> {
        match kind {
            PromptKind::Search => {
                let matched = self.active_rows().map_or(0, Rows::len);
                let matched_total = self.active_rows().map_or(0, Rows::total);
                if value.is_empty() {
                    self.set_filter("");
                    self.set_status(StatusKind::Info, "filter cleared");
                } else {
                    self.set_status(
                        StatusKind::Info,
                        format!("filtering `{value}` — {matched} of {matched_total} rows"),
                    );
                }
                Vec::new()
            }
            PromptKind::Rename => {
                let Some(uid) = self.profiles.selected_item().map(|row| row.uid.clone()) else {
                    self.refuse("no profile selected");
                    return Vec::new();
                };
                let name = value.trim();
                if name.is_empty() {
                    self.reopen_prompt(kind, value, "a profile needs a name");
                    return Vec::new();
                }
                vec![Effect::RenameProfile {
                    uid,
                    name: name.to_owned(),
                }]
            }
            PromptKind::Url => {
                let url = value.trim();
                if !url.starts_with("http://") && !url.starts_with("https://") {
                    self.reopen_prompt(
                        kind,
                        value,
                        "a subscription URL must start with http:// or https://",
                    );
                    return Vec::new();
                }
                vec![Effect::NewProfile {
                    name: name_from_url(url),
                    url: Some(url.to_owned()),
                }]
            }
            PromptKind::Name => {
                let name = value.trim();
                if name.is_empty() {
                    self.reopen_prompt(kind, value, "a profile needs a name");
                    return Vec::new();
                }
                vec![Effect::NewProfile {
                    name: name.to_owned(),
                    url: None,
                }]
            }
            PromptKind::Text => self.commit_text_setting(value),
        }
    }

    /// Refuse what was typed and leave the prompt open with it.
    ///
    /// Closing a prompt on a rejected answer throws the typing away and makes
    /// the user start again; keeping it open means one correction, not one
    /// re-entry.
    fn reopen_prompt(&mut self, kind: PromptKind, value: &str, reason: &str) {
        self.set_status(StatusKind::Warning, reason);
        let cursor = value.chars().count();
        let label = self.prompt_label(kind);
        self.overlay = Some(Overlay::Prompt {
            label,
            kind,
            value: value.to_owned(),
            cursor,
        });
    }

    fn commit_text_setting(&mut self, value: &str) -> Vec<Effect> {
        let Some(row) = self.settings_rows.selected_item() else {
            self.refuse("no setting selected");
            return Vec::new();
        };
        let key = row.key;
        if let Err(reason) = set_setting_text(&mut self.settings, key, value) {
            self.reopen_prompt(PromptKind::Text, value, &reason);
            return Vec::new();
        }
        self.settings_dirty = true;
        let effects = self.after_settings_change();
        self.set_status(StatusKind::Info, format!("{key} = {}", value.trim()));
        effects
    }

    // -- action dispatch ----------------------------------------------------

    fn dispatch(&mut self, action: Action, confirmed: bool) -> Vec<Effect> {
        if action.is_destructive() && !confirmed {
            if let Some(reason) = self.destructive_blocker(&action) {
                self.refuse(reason);
                return Vec::new();
            }
            let question = self.confirm_question(&action);
            self.overlay = Some(Overlay::Confirm { question, action });
            return Vec::new();
        }
        self.perform(&action)
    }

    /// Why a destructive action would be pointless, checked before asking.
    ///
    /// Confirming "stop the core?" when nothing is running trains the user to
    /// answer yes without reading, which is exactly what a confirmation is
    /// supposed to prevent.
    fn destructive_blocker(&self, action: &Action) -> Option<String> {
        match action {
            Action::StopCore if !self.core.is_running() => {
                Some("the core is not running".to_owned())
            }
            Action::DeleteProfile if self.profiles.is_empty() => {
                Some("there is no profile to delete".to_owned())
            }
            Action::CloseAllConnections if self.connections.is_empty() => {
                Some("there is no connection to close".to_owned())
            }
            _ => None,
        }
    }

    fn confirm_question(&self, action: &Action) -> String {
        match action {
            Action::DeleteProfile => self.profiles.selected_item().map_or_else(
                || "delete this profile?".to_owned(),
                |row| format!("delete `{}` and its document?", row.name),
            ),
            Action::CloseAllConnections => {
                format!("close all {} connection(s)?", self.connections.total())
            }
            Action::StopCore => "stop the core? nothing will be proxied".to_owned(),
            Action::RollbackConfig => {
                "restore the previous generated configuration and restart the core?".to_owned()
            }
            Action::UpgradeCore => "download and install the latest managed core?".to_owned(),
            other => format!("{}?", other.label()),
        }
    }

    /// The dispatcher: one arm per [`Action`], no exceptions.
    ///
    /// The action is borrowed because only [`Action::Goto`] carries a payload
    /// the dispatcher needs; everything else re-reads the state it acts on, so
    /// taking the value would mean moving sixty payloads nowhere.
    #[allow(clippy::too_many_lines)] // one arm per action is the readable form
    fn perform(&mut self, action: &Action) -> Vec<Effect> {
        match action {
            Action::Quit => {
                self.quit = true;
                vec![Effect::Quit]
            }
            Action::Goto(screen) => self.goto(*screen),
            Action::NextScreen => self.goto(self.screen.next()),
            Action::PreviousScreen => self.goto(self.screen.previous()),
            Action::Refresh => {
                self.remember_loaded(self.screen);
                Self::refresh_effects(self.screen)
            }
            Action::Cancel => {
                self.cancel_scope();
                Vec::new()
            }
            Action::ShowLastMessage => self.show_last_message(),
            Action::Up => self.move_cursor(-1),
            Action::Down => self.move_cursor(1),
            Action::PageUp => self.page_cursor(-1),
            Action::PageDown => self.page_cursor(1),
            Action::Top => self.move_to_edge(true),
            Action::Bottom => self.move_to_edge(false),
            Action::Search => {
                let current = self.active_rows().map_or_else(String::new, Rows::filter);
                self.open_prompt(PromptKind::Search, current);
                Vec::new()
            }
            Action::SearchNext => {
                if let Some(rows) = self.active_rows_mut() {
                    rows.next_match();
                }
                self.sync_scroll();
                Vec::new()
            }
            Action::ActivateProfile => self.activate_profile(),
            Action::UpdateProfile => self.update_profile(),
            // The executor owns the full index and due calculation; an empty
            // list requests all due profiles, including rows not yet loaded.
            Action::UpdateAllProfiles => vec![Effect::UpdateProfiles { uids: Vec::new() }],
            Action::NewProfile => {
                self.overlay = Some(Overlay::Picker {
                    title: "new profile".to_owned(),
                    items: vec!["from a URL".to_owned(), "a blank local profile".to_owned()],
                    selected: 0,
                });
                Vec::new()
            }
            Action::DeleteProfile => self.delete_profile(),
            Action::RenameProfile => {
                let Some(row) = self.profiles.selected_item() else {
                    self.refuse("no profile selected");
                    return Vec::new();
                };
                let name = row.name.clone();
                self.open_prompt(PromptKind::Rename, name);
                Vec::new()
            }
            Action::ImportProfiles => vec![Effect::DetectImportSources],
            Action::EditProfile => self.edit_profile(),
            Action::ToggleInChain => self.toggle_in_chain(),
            Action::PreviewConfig => vec![Effect::PreviewConfig],
            Action::ApplyConfig => vec![Effect::ApplyConfig {
                mode: self.reload_mode(),
            }],
            Action::RollbackConfig => vec![Effect::RollbackConfig],
            Action::SelectNode => self.select_node(),
            Action::TestGroup => self.test_group(),
            Action::TestNode => self.test_node(),
            Action::TestAllNodes => {
                if !self.require_core("testing nodes") {
                    return Vec::new();
                }
                vec![Effect::TestAllNodes]
            }
            Action::ClearNodeSelection => self.clear_node_pin(),
            Action::CloseConnection => self.close_connection(),
            Action::CloseAllConnections => {
                if !self.require_core("closing connections") {
                    return Vec::new();
                }
                vec![Effect::CloseAllConnections]
            }
            Action::CycleConnectionSort => {
                self.connection_sort = self.connection_sort.next();
                self.resort_connections();
                let label = self.connection_sort.label();
                self.set_status(StatusKind::Info, format!("connections sorted by {label}"));
                Vec::new()
            }
            Action::ToggleLogFollow => {
                self.logs.follow = !self.logs.follow;
                self.frozen = if self.logs.follow {
                    None
                } else {
                    Some(self.logs.filtered().len())
                };
                let text = if self.logs.follow {
                    "following new lines"
                } else {
                    "follow stopped; new lines are buffered but not shown"
                };
                self.set_status(StatusKind::Info, text);
                Vec::new()
            }
            Action::CycleLogLevel => self.cycle_log_level(),
            Action::ClearLogs => {
                let dropped = self.logs.len();
                self.logs.clear();
                self.set_status(StatusKind::Info, format!("discarded {dropped} log line(s)"));
                Vec::new()
            }
            Action::ExportLogs => {
                if self.logs.is_empty() {
                    self.refuse("the log buffer is empty");
                    return Vec::new();
                }
                let path = self.log_export_path();
                let contents = self.logs.export();
                vec![Effect::ExportLogs { path, contents }]
            }
            Action::ToggleRule => self.toggle_rule(),
            Action::UpdateRuleProvider => self.update_rule_provider(),
            Action::UpdateAllRuleProviders => {
                if !self.require_core("updating rule sets") {
                    return Vec::new();
                }
                vec![Effect::UpdateRuleProviders { names: Vec::new() }]
            }
            Action::ToggleDisabledRules => {
                self.show_disabled_rules = !self.show_disabled_rules;
                self.rebuild_rules();
                let text = if self.show_disabled_rules {
                    "showing disabled rules"
                } else {
                    "hiding disabled rules"
                };
                self.set_status(StatusKind::Info, text);
                Vec::new()
            }
            Action::RunTests => self.run_tests(),
            Action::CancelTests => self.cancel_tests(),
            Action::ClearTestResults => {
                self.clear_test_results();
                vec![Effect::ClearTestResults]
            }
            Action::StartCore => vec![Effect::StartCore],
            Action::StopCore => vec![Effect::StopCore],
            Action::RestartCore => {
                if !self.require_core("restarting the core") {
                    return Vec::new();
                }
                vec![Effect::RestartCore]
            }
            Action::CycleCoreMode => {
                if !self.require_core("changing the routing mode") {
                    return Vec::new();
                }
                let next = match self.core_mode.as_deref() {
                    Some("rule") => "global",
                    Some("global") => "direct",
                    _ => "rule",
                };
                vec![Effect::SetCoreMode {
                    mode: next.to_owned(),
                }]
            }
            Action::UpgradeCore => vec![Effect::UpgradeCore],
            Action::UpdateGeo => {
                if !self.require_core("updating geo databases") {
                    return Vec::new();
                }
                vec![Effect::UpdateGeo]
            }
            Action::FlushCaches => {
                if !self.require_core("flushing caches") {
                    return Vec::new();
                }
                vec![Effect::FlushCaches]
            }
            Action::EditRuntimeConfig => vec![Effect::OpenEditor {
                path: self.runtime_config_path(),
            }],
            Action::SaveSettings => vec![Effect::SaveSettings {
                settings: self.settings.clone(),
            }],
            Action::ToggleSetting => self.toggle_setting(),
        }
    }

    // -- screens and lists --------------------------------------------------

    fn goto(&mut self, screen: Screen) -> Vec<Effect> {
        self.screen = screen;
        if screen == Screen::Tests {
            self.rebuild_tests();
        }
        self.sync_scroll();
        if self.loaded.contains(&screen) {
            return Vec::new();
        }
        self.remember_loaded(screen);
        Self::refresh_effects(screen)
    }

    fn remember_loaded(&mut self, screen: Screen) {
        if !self.loaded.contains(&screen) {
            self.loaded.push(screen);
        }
    }

    fn refresh_effects(screen: Screen) -> Vec<Effect> {
        match screen {
            // The profile screen's data is the store, which has its own effect.
            Screen::Profiles => vec![Effect::LoadProfiles],
            // The key reference is generated from the key map; nothing to read.
            Screen::Help => Vec::new(),
            other => vec![Effect::Refresh(other)],
        }
    }

    fn active_rows(&self) -> Option<&dyn Rows> {
        match self.screen {
            Screen::Profiles => Some(&self.profiles),
            Screen::Proxies => Some(&self.nodes),
            Screen::Connections => Some(&self.connections),
            Screen::Rules => Some(&self.rules),
            Screen::Tests => Some(&self.tests),
            Screen::Settings => Some(&self.settings_rows),
            Screen::Logs => Some(&self.logs),
            Screen::Home | Screen::Help => None,
        }
    }

    fn active_rows_mut(&mut self) -> Option<&mut dyn Rows> {
        match self.screen {
            Screen::Profiles => Some(&mut self.profiles),
            Screen::Proxies => Some(&mut self.nodes),
            Screen::Connections => Some(&mut self.connections),
            Screen::Rules => Some(&mut self.rules),
            Screen::Tests => Some(&mut self.tests),
            Screen::Settings => Some(&mut self.settings_rows),
            Screen::Logs => Some(&mut self.logs),
            Screen::Home | Screen::Help => None,
        }
    }

    fn active_table_offset(&self) -> usize {
        match self.screen {
            Screen::Profiles => self.profiles.offset(),
            Screen::Proxies => self.nodes.offset(),
            Screen::Connections => self.connections.offset(),
            Screen::Rules => self.rules.offset(),
            Screen::Tests => self.tests.offset(),
            Screen::Settings => self.settings_rows.offset(),
            Screen::Home | Screen::Logs | Screen::Help => 0,
        }
    }

    fn select_table_row(&mut self, index: usize) {
        match self.screen {
            Screen::Profiles => self.profiles.select(index),
            Screen::Proxies => self.nodes.select(index),
            Screen::Connections => self.connections.select(index),
            Screen::Rules => self.rules.select(index),
            Screen::Tests => self.tests.select(index),
            Screen::Settings => self.settings_rows.select(index),
            Screen::Home | Screen::Logs | Screen::Help => return,
        }
        self.sync_scroll();
    }

    fn scroll_logs(&mut self, delta: isize) {
        let len = self.logs.filtered().len();
        let end = self.frozen.unwrap_or(len).min(len);
        let next = end.saturating_add_signed(delta).min(len);
        self.logs.follow = next == len;
        self.frozen = if self.logs.follow { None } else { Some(next) };
    }

    fn sync_scroll(&mut self) {
        let height = self.visible_rows();
        if let Some(rows) = self.active_rows_mut() {
            rows.scroll_into_view(height);
        }
    }

    fn move_cursor(&mut self, delta: isize) -> Vec<Effect> {
        if let Some(rows) = self.active_rows_mut() {
            rows.move_by(delta);
        }
        self.sync_scroll();
        Vec::new()
    }

    fn page_cursor(&mut self, direction: isize) -> Vec<Effect> {
        let height = self.visible_rows();
        if let Some(rows) = self.active_rows_mut() {
            rows.page(direction, height);
        }
        self.sync_scroll();
        Vec::new()
    }

    fn move_to_edge(&mut self, first: bool) -> Vec<Effect> {
        if let Some(rows) = self.active_rows_mut() {
            if first {
                rows.select_first();
            } else {
                rows.select_last();
            }
        }
        self.sync_scroll();
        Vec::new()
    }

    fn set_filter(&mut self, needle: &str) {
        if let Some(rows) = self.active_rows_mut() {
            rows.set_filter(needle);
        }
        self.sync_scroll();
    }

    fn clear_filter(&mut self) -> bool {
        let had = self.active_rows().is_some_and(|r| !r.filter().is_empty());
        if let Some(rows) = self.active_rows_mut() {
            rows.clear_filter();
        }
        self.sync_scroll();
        had
    }

    /// `Esc` with nothing else to close: clear the filter, or say so.
    fn cancel_scope(&mut self) {
        if self.clear_filter() {
            self.set_status(StatusKind::Info, "filter cleared");
        } else {
            // Esc dismisses the footer immediately; the full text remains
            // available with `m` through `last_status`.
            self.status = None;
        }
    }

    fn open_prompt(&mut self, kind: PromptKind, value: String) {
        let cursor = value.chars().count();
        let label = self.prompt_label(kind);
        self.overlay = Some(Overlay::Prompt {
            label,
            kind,
            value,
            cursor,
        });
    }

    /// Run a picker choice.
    fn choose(&mut self, title: &str, items: &[String], selected: usize) -> Vec<Effect> {
        let Some(item) = items.get(selected).cloned() else {
            self.set_status(StatusKind::Warning, "nothing to choose");
            return Vec::new();
        };
        match title {
            "new profile" => {
                if selected == 0 {
                    self.open_prompt(PromptKind::Url, String::new());
                } else {
                    self.open_prompt(PromptKind::Name, String::new());
                }
                Vec::new()
            }
            "import from" => vec![Effect::ImportProfiles {
                source: PathBuf::from(item),
            }],
            "update rule set" => vec![Effect::UpdateRuleProviders { names: vec![item] }],
            other => {
                self.set_status(
                    StatusKind::Info,
                    format!("nothing to do with `{other}` = `{item}`"),
                );
                Vec::new()
            }
        }
    }

    // -- profiles -----------------------------------------------------------

    fn activate_profile(&mut self) -> Vec<Effect> {
        let Some(row) = self.profiles.selected_item() else {
            self.refuse("no profile selected");
            return Vec::new();
        };
        if row.current {
            let name = row.name.clone();
            self.set_status(StatusKind::Info, format!("`{name}` is already current"));
            return Vec::new();
        }
        if let Some(reason) = &row.unsupported {
            let reason = reason.clone();
            self.refuse(reason);
            return Vec::new();
        }
        vec![Effect::SwitchProfile {
            uid: row.uid.clone(),
        }]
    }

    fn update_profile(&mut self) -> Vec<Effect> {
        let Some(row) = self.profiles.selected_item() else {
            self.refuse("no profile selected");
            return Vec::new();
        };
        if row.url.is_none() {
            let name = row.name.clone();
            self.refuse(format!("`{name}` is local; there is nothing to download"));
            return Vec::new();
        }
        vec![Effect::UpdateProfiles {
            uids: vec![row.uid.clone()],
        }]
    }

    fn delete_profile(&mut self) -> Vec<Effect> {
        let Some(row) = self.profiles.selected_item() else {
            self.refuse("no profile selected");
            return Vec::new();
        };
        vec![Effect::DeleteProfile {
            uid: row.uid.clone(),
        }]
    }

    fn edit_profile(&mut self) -> Vec<Effect> {
        let Some(row) = self.profiles.selected_item() else {
            self.refuse("no profile selected");
            return Vec::new();
        };
        vec![Effect::EditProfile {
            uid: row.uid.clone(),
        }]
    }

    fn toggle_in_chain(&mut self) -> Vec<Effect> {
        let Some(row) = self.profiles.selected_item().cloned() else {
            self.refuse("no profile selected");
            return Vec::new();
        };
        if row.kind.is_base() {
            self.refuse(format!(
                "`{}` supplies the base document, so it is always applied; only patches can be chained",
                row.name
            ));
            return Vec::new();
        }
        if let Some(reason) = &row.unsupported {
            let reason = reason.clone();
            self.refuse(reason);
            return Vec::new();
        }
        let text = if self.chain.contains(&row.uid) {
            self.chain.retain(|uid| *uid != row.uid);
            format!("`{}` removed from the chain", row.name)
        } else {
            self.chain.push(row.uid.clone());
            format!("`{}` added to the chain", row.name)
        };
        self.mark_chain();
        self.set_status(StatusKind::Info, text);
        vec![Effect::SetChain {
            uids: self.chain.clone(),
        }]
    }

    /// Reflect the local chain list in the rows, so the list updates at once
    /// instead of waiting for the store to be re-read.
    fn mark_chain(&mut self) {
        let chain = self.chain.clone();
        let key = self.profiles.selected_item().map(|row| row.uid.clone());
        self.map_profiles(|row| row.in_chain = chain.contains(&row.uid));
        if let Some(key) = key {
            self.profiles.select_by_key(key, |row| row.uid.clone());
        }
    }

    fn map_profiles(&mut self, mut f: impl FnMut(&mut ProfileRow)) {
        let key = self.profiles.selected_item().map(|row| row.uid.clone());
        let mut items = self.profiles.items().to_vec();
        for row in &mut items {
            f(row);
        }
        self.profiles.set_items(items);
        if let Some(key) = key {
            self.profiles.select_by_key(key, |row| row.uid.clone());
        }
    }

    // -- proxies ------------------------------------------------------------

    fn toggle_group(&mut self, group: &str) -> Vec<Effect> {
        if let Some(position) = self.expanded.iter().position(|g| g == group) {
            self.expanded.remove(position);
        } else {
            self.expanded.push(group.to_owned());
        }
        self.rebuild_nodes();
        Vec::new()
    }

    fn select_node(&mut self) -> Vec<Effect> {
        let Some(row) = self.nodes.selected_item().cloned() else {
            self.refuse("no node selected");
            return Vec::new();
        };
        if row.is_group {
            if row.members == 0 {
                self.refuse(format!("`{}` has no members to show", row.name));
                return Vec::new();
            }
            return self.toggle_group(&row.name);
        }
        let Some(group) = row.group.clone() else {
            self.refuse(format!("`{}` is not a member of a group", row.name));
            return Vec::new();
        };
        if !self.require_core("selecting a node") {
            return Vec::new();
        }
        vec![Effect::SelectNode {
            group,
            member: row.name,
        }]
    }

    /// The group a highlighted row belongs to: itself when it is a group.
    fn highlighted_group(&self) -> Option<String> {
        self.nodes
            .selected_item()
            .map(|row| row.group.clone().unwrap_or_else(|| row.name.clone()))
    }

    fn test_node(&mut self) -> Vec<Effect> {
        let Some(row) = self.nodes.selected_item().cloned() else {
            self.refuse("no node selected");
            return Vec::new();
        };
        let Some(_group) = row.group.clone() else {
            let name = row.name.clone();
            return if row.is_group {
                self.test_group_named(name)
            } else {
                self.refuse(format!("`{name}` is not part of a group"));
                Vec::new()
            };
        };
        if !self.require_core("testing a node") {
            return Vec::new();
        }
        vec![Effect::TestNode { name: row.name }]
    }

    fn test_group(&mut self) -> Vec<Effect> {
        let Some(group) = self.highlighted_group() else {
            self.refuse("no group selected");
            return Vec::new();
        };
        self.test_group_named(group)
    }

    fn test_group_named(&mut self, group: String) -> Vec<Effect> {
        if !self.require_core("testing a group") {
            return Vec::new();
        }
        vec![Effect::TestGroup { group }]
    }

    fn clear_node_pin(&mut self) -> Vec<Effect> {
        let Some(row) = self.nodes.selected_item().cloned() else {
            self.refuse("no node selected");
            return Vec::new();
        };
        let group = row.group.clone().unwrap_or_else(|| row.name.clone());
        if row.is_group && !row.selectable {
            self.refuse(format!("`{group}` does not choose a node by hand"));
            return Vec::new();
        }
        if !self.require_core("clearing a pin") {
            return Vec::new();
        }
        vec![Effect::ClearNodePin { group }]
    }

    // -- connections --------------------------------------------------------

    fn close_connection(&mut self) -> Vec<Effect> {
        let Some(id) = self.connections.selected_item().map(|row| row.id.clone()) else {
            self.refuse("no connection selected");
            return Vec::new();
        };
        if !self.require_core("closing a connection") {
            return Vec::new();
        }
        vec![Effect::CloseConnection { id }]
    }

    // -- logs ---------------------------------------------------------------

    fn cycle_log_level(&mut self) -> Vec<Effect> {
        let all = LogLevel::all();
        let position = all.iter().position(|l| *l == self.log_level).unwrap_or(0);
        let next = all[(position + 1) % all.len()];
        self.log_level = next;
        self.settings.ui.log_level = next;
        self.settings_dirty = true;
        self.rebuild_settings_rows();
        let label = next.as_str();
        if self.core.is_running() {
            self.set_status(StatusKind::Info, format!("core log level is now {label}"));
            vec![Effect::SetCoreLogLevel(next)]
        } else {
            self.set_status(
                StatusKind::Info,
                format!("showing {label} and above; the core is not running"),
            );
            Vec::new()
        }
    }

    // -- rules --------------------------------------------------------------

    fn toggle_rule(&mut self) -> Vec<Effect> {
        let Some(row) = self
            .rules
            .selected_item()
            .map(|row| (row.index, row.disabled))
        else {
            self.refuse("no rule selected");
            return Vec::new();
        };
        if !self.require_core("changing a rule") {
            return Vec::new();
        }
        vec![Effect::ToggleRule {
            index: row.0,
            disabled: !row.1,
        }]
    }

    fn update_rule_provider(&mut self) -> Vec<Effect> {
        if !self.require_core("updating a rule set") {
            return Vec::new();
        }
        if self.rule_providers.is_empty() {
            // Rules carry no provider name, so there is nothing to narrow the
            // request to; say so rather than pretending a single set was meant.
            self.set_status(
                StatusKind::Info,
                "the core lists no rule providers; updating every set",
            );
            return vec![Effect::UpdateRuleProviders { names: Vec::new() }];
        }
        if self.rule_providers.len() == 1 {
            let names = vec![self.rule_providers[0].clone()];
            return vec![Effect::UpdateRuleProviders { names }];
        }
        self.overlay = Some(Overlay::Picker {
            title: "update rule set".to_owned(),
            items: self.rule_providers.clone(),
            selected: 0,
        });
        Vec::new()
    }

    // -- tests --------------------------------------------------------------

    /// Rebuild the tests screen from what the other screens currently show.
    ///
    /// Results are kept for a target that has not changed: re-entering the
    /// screen must not wipe a measurement the user just waited for.
    fn rebuild_tests(&mut self) {
        let group = self
            .nodes
            .items()
            .iter()
            .find(|row| row.is_group)
            .map_or_else(|| "-".to_owned(), |row| row.name.clone());
        let node = self
            .nodes
            .items()
            .iter()
            .find(|row| !row.is_group)
            .map_or_else(|| "-".to_owned(), |row| row.name.clone());
        let host = self
            .connections
            .items()
            .first()
            .map_or_else(|| "-".to_owned(), |row| row.destination.clone());
        let targets = [
            (TestKind::GroupLatency, group),
            (TestKind::NodeLatency, node),
            (TestKind::CoreHealth, "core".to_owned()),
            (TestKind::DnsLookup, host),
        ];
        let selected = self
            .tests
            .selected_item()
            .map(|row| (row.kind, row.target.clone()));
        let previous = self.tests.items().to_vec();
        let rows: Vec<TestRow> = targets
            .into_iter()
            .map(|(kind, target)| {
                let result = previous
                    .iter()
                    .find(|row| row.kind == kind && row.target == target)
                    .map_or(TestResult::Pending, |row| row.result.clone());
                TestRow {
                    kind,
                    target,
                    result,
                }
            })
            .collect();
        let filter = self.tests.filter().to_owned();
        self.tests = Table::from_items(rows);
        if !filter.is_empty() {
            self.tests.set_filter(&filter);
        }
        // Arriving data can rebuild these rows several times a second, so the
        // cursor has to follow the row it was on rather than the position.
        if let Some((kind, target)) = selected
            && let Some(position) = self
                .tests
                .items()
                .iter()
                .position(|row| row.kind == kind && row.target == target)
        {
            self.tests.select(position);
        }
    }

    fn run_tests(&mut self) -> Vec<Effect> {
        let Some(index) = self.tests.selected_source_index() else {
            self.refuse("no test selected");
            return Vec::new();
        };
        // A queued test is a request that has been accepted; only a test that
        // already ran (or is in flight) is worth refusing.
        if matches!(
            self.tests.items()[index].result,
            TestResult::Running | TestResult::Passed(_) | TestResult::Failed(_)
        ) {
            let label = self.tests.items()[index].kind.label();
            self.refuse(format!("`{label}` has already run; press c to clear it"));
            return Vec::new();
        }
        if !self.require_core("running a test") {
            return Vec::new();
        }
        let row = self.tests.items()[index].clone();
        self.set_test_result(index, TestResult::Running);
        if self.in_flight.is_some() {
            self.queue.push_back(index);
            let label = row.kind.label();
            let batch = self.queued_tests();
            self.set_status(
                StatusKind::Info,
                format!("queued `{label}` ({batch} in the batch)"),
            );
            return Vec::new();
        }
        self.in_flight = Some(index);
        vec![Effect::RunTest {
            kind: row.kind,
            target: row.target,
        }]
    }

    fn cancel_tests(&mut self) -> Vec<Effect> {
        if self.in_flight.is_none() && self.queue.is_empty() {
            self.refuse("no test batch is running");
            return Vec::new();
        }
        let count = self.queued_tests();
        self.abandon_tests();
        self.set_status(StatusKind::Warning, format!("cancelled {count} test(s)"));
        vec![Effect::CancelTests]
    }

    /// Put every queued or in-flight test back to pending.
    fn abandon_tests(&mut self) {
        let running = self.in_flight.take();
        for index in running.into_iter().chain(std::mem::take(&mut self.queue)) {
            self.set_test_result(index, TestResult::Pending);
        }
    }

    fn clear_test_results(&mut self) {
        self.abandon_tests();
        for index in 0..self.tests.items().len() {
            self.set_test_result(index, TestResult::Pending);
        }
    }

    /// Set one test's result, keeping the cursor on the row it was on.
    fn set_test_result(&mut self, index: usize, result: TestResult) {
        let key = self
            .tests
            .selected_source_index()
            .and_then(|i| self.tests.items().get(i))
            .map(|row| (row.kind, row.target.clone()));
        let mut items = self.tests.items().to_vec();
        if let Some(row) = items.get_mut(index) {
            row.result = result;
        }
        self.tests.set_items(items);
        if let Some((kind, target)) = key
            && let Some(position) = self
                .tests
                .items()
                .iter()
                .position(|row| row.kind == kind && row.target == target)
        {
            self.tests.select(position);
        }
    }

    fn on_test_result(&mut self, kind: TestKind, target: &str, result: TestResult) -> Vec<Effect> {
        let index = self
            .tests
            .items()
            .iter()
            .position(|row| row.kind == kind && row.target == target);
        if let Some(index) = index {
            self.set_test_result(index, result);
            self.queue.retain(|queued| *queued != index);
            if self.in_flight == Some(index) {
                self.in_flight = None;
            }
        }
        // The next queued test starts as soon as the previous one answers,
        // which is what keeps a batch from opening every node at once.
        while let Some(next) = self.queue.pop_front() {
            if let Some(row) = self.tests.items().get(next) {
                let (kind, target) = (row.kind, row.target.clone());
                self.in_flight = Some(next);
                return vec![Effect::RunTest { kind, target }];
            }
        }
        Vec::new()
    }

    // -- settings -----------------------------------------------------------

    fn toggle_setting(&mut self) -> Vec<Effect> {
        let Some(row) = self.settings_rows.selected_item().cloned() else {
            self.refuse("no setting selected");
            return Vec::new();
        };
        let key = row.key;
        if row.kind == SettingKind::Text {
            // The editable form, never the rendering.
            let current = row.editable.clone();
            self.open_prompt(PromptKind::Text, current);
            return Vec::new();
        }
        if !cycle_setting(&mut self.settings, key, true) {
            self.refuse(format!("`{key}` cannot be changed here"));
            return Vec::new();
        }
        self.settings_dirty = true;
        let effects = self.after_settings_change();
        let value = self
            .settings_rows
            .items()
            .iter()
            .find(|r| r.key == key)
            .map_or_else(String::new, |r| r.value.clone());
        self.set_status(StatusKind::Info, format!("{} = {value}", row.label));
        effects
    }

    /// Apply a settings change to everything it affects.
    fn after_settings_change(&mut self) -> Vec<Effect> {
        self.theme = Theme::from_settings(self.settings.ui.color);
        self.rebuild_settings_rows();
        if self.settings.ui.log_level == self.log_level {
            return Vec::new();
        }
        self.log_level = self.settings.ui.log_level;
        if self.core.is_running() {
            return vec![Effect::SetCoreLogLevel(self.log_level)];
        }
        Vec::new()
    }

    fn rebuild_settings_rows(&mut self) {
        let key = self
            .settings_rows
            .selected_item()
            .map(|row| row.key.to_owned());
        let filter = self.settings_rows.filter().to_owned();
        self.settings_rows = Table::from_items(setting_rows(&self.settings));
        if !filter.is_empty() {
            self.settings_rows.set_filter(&filter);
        }
        if let Some(key) = key {
            self.settings_rows
                .select_by_key(key, |row| row.key.to_owned());
        }
    }

    // -- data arriving ------------------------------------------------------

    fn on_data(&mut self, data: Data) -> Vec<Effect> {
        match data {
            Data::Profiles(rows) => self.set_profiles(rows),
            Data::Nodes(rows) => self.set_nodes(rows),
            Data::Connections(rows) => self.set_connections(rows),
            Data::Rules(rows) => self.set_rules(rows),
            Data::RuleProviders(names) => self.rule_providers = names,
            Data::Log(row) => self.logs.push(row),
            Data::Traffic(sample) => self.metrics.push_traffic(sample),
            Data::Memory(bytes) => self.metrics.push_memory(bytes),
            Data::Core(status) => {
                let was_running = self.core.is_running();
                self.core = status;
                if was_running && !self.core.is_running() {
                    // Nothing can be running against a core that is gone.
                    self.abandon_tests();
                    self.core_mode = None;
                    self.invalidate_controller_views();
                    return vec![Effect::Refresh(Screen::Proxies)];
                }
                if !was_running && self.core.is_running() {
                    self.invalidate_controller_views();
                    return vec![
                        Effect::Refresh(Screen::Proxies),
                        Effect::Refresh(Screen::Rules),
                        Effect::Refresh(Screen::Connections),
                    ];
                }
            }
            Data::CoreMode(mode) => self.core_mode = Some(mode),
            Data::Version(version) => self.version = Some(version),
            Data::Settings(settings) => self.set_settings(*settings),
            Data::Preview(preview) => {
                let lines = preview.lines();
                self.preview = Some(*preview);
                self.overlay = Some(Overlay::Preview {
                    title: "generated configuration".to_owned(),
                    lines,
                    scroll: 0,
                });
            }
            Data::ImportSources(sources) => return self.open_import_picker(&sources),
            Data::TestResult {
                kind,
                target,
                result,
            } => return self.on_test_result(kind, &target, result),
            Data::Notice(text) => self.set_status(StatusKind::Info, text),
        }
        Vec::new()
    }

    fn open_import_picker(&mut self, sources: &[PathBuf]) -> Vec<Effect> {
        if sources.is_empty() {
            self.set_status(
                StatusKind::Warning,
                "no clash-verge-rev installation was found to import from",
            );
            return Vec::new();
        }
        let items: Vec<String> = sources.iter().map(|p| p.display().to_string()).collect();
        self.overlay = Some(Overlay::Picker {
            title: "import from".to_owned(),
            items,
            selected: 0,
        });
        Vec::new()
    }

    fn set_profiles(&mut self, rows: Vec<ProfileRow>) {
        let previous = self
            .profiles
            .selected_item()
            .map(|row| row.uid.clone())
            .or_else(|| {
                rows.iter()
                    .find(|row| row.current)
                    .map(|row| row.uid.clone())
            });
        let base = rows
            .iter()
            .find(|row| row.current)
            .map(|row| row.uid.clone());
        let chain: Vec<String> = rows
            .iter()
            .filter(|row| row.in_chain)
            .map(|row| row.uid.clone())
            .collect();
        if !chain.is_empty() || self.chain.is_empty() {
            self.chain = chain;
        }
        self.profiles.set_items(rows);
        if let Some(uid) = previous.or(base) {
            self.profiles.select_by_key(uid, |row| row.uid.clone());
        }
    }

    fn set_nodes(&mut self, rows: Vec<NodeRow>) {
        self.all_nodes = rows;
        self.rebuild_nodes();
        self.refresh_test_targets();
    }

    fn invalidate_controller_views(&mut self) {
        self.all_nodes.clear();
        self.nodes.set_items(Vec::new());
        self.expanded.clear();
        self.connections.set_items(Vec::new());
        self.all_rules.clear();
        self.rules.set_items(Vec::new());
        self.rule_providers.clear();
        self.loaded.retain(|screen| {
            !matches!(
                screen,
                Screen::Proxies | Screen::Connections | Screen::Rules
            )
        });
    }

    /// Flatten the node tree to what the expansion state shows.
    ///
    /// Groups are collapsed the first time they are seen: a subscription can
    /// hold hundreds of nodes, and a list that starts as a wall of them hides
    /// the group the user is looking for.
    fn rebuild_nodes(&mut self) {
        let selected = self.nodes.selected_item().map(|row| row.name.clone());
        let expanded = self.expanded.clone();
        let rows: Vec<NodeRow> = self
            .all_nodes
            .iter()
            .filter(|row| {
                row.group
                    .as_deref()
                    .is_none_or(|group| expanded.iter().any(|e| e == group))
            })
            .cloned()
            .collect();
        self.nodes.set_items(rows);
        if let Some(name) = selected {
            self.nodes.select_by_key(name, |row| row.name.clone());
        }
        let order = self.node_sort;
        if order != SortOrder::Natural {
            self.sort_nodes(order);
        }
    }

    /// Order nodes by latency, keeping each group's members together.
    fn sort_nodes(&mut self, order: SortOrder) {
        self.node_sort = order;
        if order == SortOrder::Natural {
            return;
        }
        let selected = self.nodes.selected_item().map(|row| row.name.clone());
        let mut rows = self.nodes.items().to_vec();
        let compare = |a: &NodeRow, b: &NodeRow| match order {
            SortOrder::LatencyDescending => b.delay.cmp(&a.delay),
            _ => a.delay.cmp(&b.delay),
        };
        rows.sort_by(|a, b| {
            let key_a = (a.group.clone(), a.is_group);
            let key_b = (b.group.clone(), b.is_group);
            key_a.cmp(&key_b).then_with(|| compare(a, b))
        });
        self.nodes.set_items(rows);
        if let Some(name) = selected {
            self.nodes.select_by_key(name, |row| row.name.clone());
        }
    }

    fn set_connections(&mut self, rows: Vec<ConnectionRow>) {
        let previous = self.connections.selected_item().map(|row| row.id.clone());
        let count = rows.len();
        self.connections.set_items(rows);
        if let Some(id) = previous {
            self.connections.select_by_key(id, |row| row.id.clone());
        }
        self.metrics.set_connections(count);
        self.resort_connections();
        self.refresh_test_targets();
    }

    /// Keep the tests screen pointed at what the other screens now show.
    fn refresh_test_targets(&mut self) {
        if self.screen == Screen::Tests {
            self.rebuild_tests();
        }
    }

    fn resort_connections(&mut self) {
        let order = self.connection_sort;
        if order == ConnectionSort::Natural {
            return;
        }
        let selected = self.connections.selected_item().map(|row| row.id.clone());
        let mut rows = self.connections.items().to_vec();
        match order {
            ConnectionSort::Busiest => rows.sort_by_key(|row| std::cmp::Reverse(row.total())),
            ConnectionSort::Oldest => rows.sort_by(|a, b| a.started.cmp(&b.started)),
            ConnectionSort::Newest => rows.sort_by(|a, b| b.started.cmp(&a.started)),
            ConnectionSort::Natural => {}
        }
        self.connections.set_items(rows);
        if let Some(id) = selected {
            self.connections.select_by_key(id, |row| row.id.clone());
        }
    }

    fn set_rules(&mut self, rows: Vec<RuleRow>) {
        self.all_rules = rows;
        self.rebuild_rules();
    }

    /// Show the rules that are currently relevant.
    fn rebuild_rules(&mut self) {
        let selected = self.rules.selected_item().map(|row| row.index);
        let show_disabled = self.show_disabled_rules;
        let rows: Vec<RuleRow> = self
            .all_rules
            .iter()
            .filter(|row| show_disabled || !row.disabled)
            .cloned()
            .collect();
        self.rules.set_items(rows);
        if let Some(index) = selected {
            self.rules.select_by_key(index, |row| row.index);
        }
    }

    fn set_settings(&mut self, settings: Settings) {
        // The disk's copy — unless the user has edits it would throw away.
        //
        // Replacing them and clearing the dirty flag made the interface report
        // a save that wrote nothing: the screen showed the edit, `s` said
        // "settings saved", and what reached the file was the copy from disk.
        // Nothing on the screen said the edit was gone, which is the part that
        // makes it a defect rather than a policy.
        //
        // Keeping the user's copy is the priority: unsaved work is the thing
        // that cannot be recovered by looking again.
        if self.settings_dirty {
            self.set_status(
                StatusKind::Warning,
                "the settings on disk changed; your edits are kept — `s` writes them",
            );
            return;
        }
        self.log_level = settings.ui.log_level;
        self.theme = Theme::from_settings(settings.ui.color && self.theme.color);
        self.settings = settings;
        self.settings_dirty = false;
        self.rebuild_settings_rows();
    }

    // -- operations finishing -----------------------------------------------

    fn on_done(&mut self, done: Done) -> Vec<Effect> {
        match done {
            Done::ProfilesLoaded => self.set_status(StatusKind::Success, "profiles loaded"),
            Done::ProfileSwitched { name } => {
                self.set_status(StatusKind::Success, format!("switched to `{name}`"));
                self.invalidate_controller_views();
                return vec![
                    Effect::LoadProfiles,
                    if self.core.is_running() {
                        Effect::ApplyConfig {
                            mode: self.reload_mode(),
                        }
                    } else {
                        Effect::PrepareConfig
                    },
                ];
            }
            Done::ProfilesUpdated { updated, failed } => {
                let kind = if failed == 0 {
                    StatusKind::Success
                } else {
                    StatusKind::Warning
                };
                self.set_status(
                    kind,
                    format!("{updated} profile(s) updated, {failed} failed"),
                );
                return vec![Effect::LoadProfiles];
            }
            Done::ChainSaved => self.set_status(StatusKind::Success, "chain saved"),
            Done::ConfigApplied { reload, changed } => {
                match reload {
                    Some(outcome) if outcome.succeeded() => self.set_status(
                        StatusKind::Success,
                        format!("{} ({changed} change(s))", outcome.summary()),
                    ),
                    Some(outcome) => {
                        self.set_status(StatusKind::Error, outcome.summary());
                    }
                    None => self.set_status(
                        StatusKind::Success,
                        format!(
                            "configuration written ({changed} change(s)); the core is not running"
                        ),
                    ),
                }
                return vec![
                    Effect::Refresh(Screen::Home),
                    Effect::Refresh(Screen::Proxies),
                    Effect::Refresh(Screen::Rules),
                    Effect::Refresh(Screen::Connections),
                ];
            }
            Done::ConfigRolledBack { snapshot } => self.set_status(
                StatusKind::Warning,
                format!("restored {}", snapshot.display()),
            ),
            Done::ProfileDeleted { name, was_current } => {
                self.loaded.retain(|&s| s != Screen::Profiles);
                self.set_status(StatusKind::Success, format!("deleted `{name}`"));
                if was_current {
                    self.invalidate_controller_views();
                    if self.core.is_running() {
                        return vec![Effect::LoadProfiles, Effect::StopCore];
                    }
                    return vec![Effect::LoadProfiles, Effect::Refresh(Screen::Proxies)];
                }
                return vec![Effect::LoadProfiles];
            }
            Done::ProfileRenamed { name } => {
                self.loaded.retain(|&s| s != Screen::Profiles);
                self.set_status(StatusKind::Success, format!("renamed to `{name}`"));
                return vec![Effect::LoadProfiles];
            }
            Done::ProfileCreated {
                name,
                uid,
                is_remote,
            } => {
                self.loaded.retain(|&s| s != Screen::Profiles);
                self.set_status(StatusKind::Success, format!("created `{name}`"));
                let mut effects = vec![Effect::LoadProfiles];
                if is_remote && let Some(uid) = uid {
                    effects.push(Effect::UpdateProfiles { uids: vec![uid] });
                }
                return effects;
            }
            Done::ProfilesImported { count } => {
                self.loaded.retain(|&s| s != Screen::Profiles);
                self.set_status(StatusKind::Success, format!("imported {count} profile(s)"));
                return vec![Effect::LoadProfiles];
            }
            Done::NodeSelected { group, member } => {
                for row in &mut self.all_nodes {
                    if row.group.as_deref() == Some(group.as_str()) {
                        row.active = row.name == member;
                    }
                }
                self.rebuild_nodes();
                self.set_status(
                    StatusKind::Success,
                    format!("`{group}` now uses `{member}`"),
                );
                return vec![Effect::Refresh(Screen::Proxies)];
            }
            Done::NodeCleared { group } => {
                for row in &mut self.all_nodes {
                    if row.group.as_deref() == Some(group.as_str()) {
                        row.active = false;
                    }
                }
                self.rebuild_nodes();
                self.set_status(
                    StatusKind::Success,
                    format!("`{group}` chooses automatically again"),
                );
                return vec![Effect::Refresh(Screen::Proxies)];
            }
            Done::NodeTestsFinished { tested } => {
                self.set_status(StatusKind::Success, format!("measured {tested} node(s)"));
                self.sort_nodes(SortOrder::LatencyAscending);
            }
            Done::ConnectionClosed => self.set_status(StatusKind::Success, "connection closed"),
            Done::ConnectionsClosed { count } => {
                self.set_status(StatusKind::Success, format!("closed {count} connection(s)"));
            }
            Done::RuleToggled { index, disabled } => {
                let state = if disabled { "disabled" } else { "enabled" };
                self.set_status(StatusKind::Success, format!("rule {index} {state}"));
            }
            Done::RuleProvidersUpdated { count } => {
                self.set_status(StatusKind::Success, format!("updated {count} rule set(s)"));
            }
            Done::CoreStarted { pid } => {
                self.set_status(StatusKind::Success, format!("core started (pid {pid})"));
                return vec![
                    Effect::Refresh(Screen::Home),
                    Effect::Refresh(Screen::Proxies),
                ];
            }
            Done::CoreStopped => {
                self.set_status(StatusKind::Success, "core stopped");
                self.invalidate_controller_views();
                return vec![
                    Effect::Refresh(Screen::Home),
                    Effect::Refresh(Screen::Proxies),
                ];
            }
            Done::CoreRestarted { pid } => {
                self.set_status(StatusKind::Success, format!("core restarted (pid {pid})"));
                return vec![
                    Effect::Refresh(Screen::Home),
                    Effect::Refresh(Screen::Proxies),
                ];
            }
            Done::CoreModeChanged { mode } => {
                self.core_mode = Some(mode.clone());
                self.set_status(StatusKind::Success, format!("routing mode: {mode}"));
                return vec![Effect::Refresh(Screen::Proxies)];
            }
            Done::CoreUpgraded { version } => {
                self.set_status(
                    StatusKind::Success,
                    format!("installed mihomo {version} (managed)"),
                );
                return vec![Effect::Refresh(Screen::Home)];
            }
            Done::GeoUpdated => self.set_status(StatusKind::Success, "geo databases updated"),
            Done::CachesFlushed => self.set_status(StatusKind::Success, "caches flushed"),
            Done::SettingsSaved => {
                self.settings_dirty = false;
                self.set_status(StatusKind::Success, "settings saved");
            }
            Done::LogsExported { path } => self.set_status(
                StatusKind::Success,
                format!("logs written to {}", path.display()),
            ),
            Done::EditorOpened { target } => {
                self.set_status(StatusKind::Info, format!("opened {target} in $EDITOR"));
            }
        }
        Vec::new()
    }

    // -- small helpers ------------------------------------------------------

    /// Refuse something the user asked for, with the reason.
    fn refuse(&mut self, reason: impl Into<String>) {
        self.set_status(StatusKind::Warning, reason);
    }

    /// Whether the core is up, refusing the request when it is not.
    fn require_core(&mut self, what: &str) -> bool {
        if self.core.is_running() {
            return true;
        }
        self.refuse(format!(
            "{what} needs a running core; press `s` on Home to start it"
        ));
        false
    }

    fn set_status(&mut self, kind: StatusKind, text: impl Into<String>) {
        let text = text.into();
        let status = Status::new(kind, text);
        self.last_status = Some(status.clone());
        self.status = Some(status);
    }

    /// Open a modal overlay showing the full text of the latest status message.
    pub fn show_last_message(&mut self) -> Vec<Effect> {
        if let Some(status) = self.last_status() {
            let formatted = self.format_status_text(&status.text);
            self.overlay = Some(Overlay::Message {
                title: self.tr("message").to_owned(),
                text: formatted,
                kind: status.kind,
                scroll: 0,
            });
        } else {
            self.overlay = Some(Overlay::Message {
                title: self.tr("message").to_owned(),
                text: self.tr("no message to show").to_owned(),
                kind: StatusKind::Info,
                scroll: 0,
            });
        }
        Vec::new()
    }

    fn expire_status(&mut self) {
        self.expire_status_at(Instant::now());
    }

    /// Drop a transient message once it is older than [`STATUS_TTL`].
    ///
    /// Takes the clock as a parameter so the rule can be tested without
    /// sleeping for four seconds.
    fn expire_status_at(&mut self, now: Instant) {
        if self.status.as_ref().is_some_and(|s| s.is_expired_at(now)) {
            self.status = None;
        }
    }

    fn reload_mode(&self) -> ReloadMode {
        if self.settings.update.prefer_hot_reload {
            ReloadMode::Auto
        } else {
            ReloadMode::Restart
        }
    }
}

/// How many log lines are kept.
const LOG_CAPACITY: usize = 5_000;

/// How many traffic and memory samples the dashboard gauges hold.
const METRIC_SAMPLES: usize = 120;

/// The byte offset of character `index`, or the end of the string.
///
/// Prompts count the caret in characters because that is what the user sees;
/// `String::insert` needs a byte offset, and a multi-byte character in a
/// subscription URL must not split.
fn char_to_byte(text: &str, index: usize) -> usize {
    text.char_indices()
        .nth(index)
        .map_or(text.len(), |(offset, _)| offset)
}

/// A name for a profile created from a URL.
fn name_from_url(url: &str) -> String {
    let trimmed = url
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let host = trimmed.split('/').next().unwrap_or_default();
    let host = host.rsplit('@').next().unwrap_or(host);
    if host.is_empty() {
        "subscription".to_owned()
    } else {
        host.to_owned()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use cvt_core::profile::item::{PrfItem, ProfileType};

    // -- fixtures -----------------------------------------------------------

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// Send a key press, and collect whatever the machine wants done.
    fn press(app: &mut App, code: KeyCode) -> Vec<Effect> {
        app.on_event(Event::Key(key(code)))
    }

    fn typed(app: &mut App, text: &str) -> Vec<Effect> {
        text.chars()
            .flat_map(|c| press(app, KeyCode::Char(c)))
            .collect()
    }

    fn app() -> App {
        App::new(PathBuf::from("/tmp/cvt-test-home"), Theme::default())
    }

    /// Jump to a screen the way the user does, ignoring the first-visit load.
    fn goto(app: &mut App, screen: Screen) {
        let digit = char::from(b'1' + u8::try_from(index_of(screen)).unwrap());
        let _ = press(app, KeyCode::Char(digit));
        assert_eq!(app.screen, screen);
    }

    fn index_of(screen: Screen) -> usize {
        Screen::all().iter().position(|s| *s == screen).unwrap()
    }

    fn profile(uid: &str, name: &str, url: Option<&str>) -> ProfileRow {
        let item = match url {
            Some(url) => PrfItem::remote(uid, name, url),
            None => PrfItem::local(uid, name),
        };
        ProfileRow::from_item(&item, false, false)
    }

    fn patch(uid: &str, name: &str, kind: ProfileType) -> ProfileRow {
        ProfileRow::from_item(&PrfItem::patch(uid, name, kind), false, false)
    }

    fn node(name: &str, group: Option<&str>, is_group: bool, delay: Option<u16>) -> NodeRow {
        NodeRow {
            name: name.to_owned(),
            kind: if is_group { "Selector" } else { "Vless" }.to_owned(),
            group: group.map(str::to_owned),
            delay,
            alive: true,
            active: false,
            is_group,
            members: if is_group { 2 } else { 0 },
            selectable: is_group,
        }
    }

    fn connection(id: &str, destination: &str, upload: u64) -> ConnectionRow {
        ConnectionRow {
            id: id.to_owned(),
            destination: destination.to_owned(),
            network: "tcp".to_owned(),
            process: "curl".to_owned(),
            rule: "MATCH".to_owned(),
            chain: "PROXY".to_owned(),
            upload,
            download: 0,
            started: "10:00:00".to_owned(),
        }
    }

    fn rule(index: u32, disabled: bool) -> RuleRow {
        RuleRow {
            index,
            kind: "DOMAIN".to_owned(),
            payload: format!("host{index}.test"),
            policy: "PROXY".to_owned(),
            disabled,
            hits: u64::from(index),
            misses: 0,
            raw: format!("DOMAIN,host{index}.test,PROXY"),
        }
    }

    fn core_running(app: &mut App) {
        let _ = app.on_event(Event::Data(Data::Core(CoreStatus::Running {
            pid: 4321,
            since: 0,
        })));
    }

    /// An application with a little of everything, and a running core.
    fn loaded() -> App {
        let mut a = app();
        core_running(&mut a);
        let _ = a.on_event(Event::Data(Data::Profiles(vec![
            ProfileRow::from_item(&PrfItem::local("base", "base"), true, false),
            patch("patch", "merge", ProfileType::Merge),
        ])));
        let _ = a.on_event(Event::Data(Data::Nodes(vec![
            node("PROXY", None, true, None),
            node("JP 01", Some("PROXY"), false, Some(20)),
            node("US 01", Some("PROXY"), false, Some(300)),
        ])));
        let _ = a.on_event(Event::Data(Data::Connections(vec![
            connection("c1", "a.test:443", 10),
            connection("c2", "b.test:443", 2048),
        ])));
        let _ = a.on_event(Event::Data(Data::Rules(vec![
            rule(0, false),
            rule(1, true),
        ])));
        a
    }

    // -- key routing --------------------------------------------------------

    #[test]
    fn a_quit_key_stops_the_application() {
        let mut a = app();
        assert!(!a.is_quit());
        assert_eq!(press(&mut a, KeyCode::Char('q')), vec![Effect::Quit]);
        assert!(a.is_quit());
    }

    #[test]
    fn ctrl_c_quits_from_every_screen() {
        for screen in Screen::all() {
            let mut a = app();
            goto(&mut a, Screen::Home);
            a.screen = screen;
            let effects = a.on_event(Event::Key(KeyEvent::new(
                KeyCode::Char('c'),
                KeyModifiers::CONTROL,
            )));
            assert_eq!(effects, vec![Effect::Quit], "{screen}");
        }
    }

    #[test]
    fn an_overlay_takes_the_key_before_the_key_map_does() {
        let mut a = loaded();
        goto(&mut a, Screen::Profiles);
        assert_eq!(press(&mut a, KeyCode::Char('/')), Vec::new());
        // `q` types a `q` into the prompt instead of quitting.
        assert_eq!(typed(&mut a, "q"), Vec::new());
        assert!(!a.is_quit(), "a prompt must absorb `q`");
        assert_eq!(
            a.overlay,
            Some(Overlay::Prompt {
                label: "filter".to_owned(),
                kind: PromptKind::Search,
                value: "q".to_owned(),
                cursor: 1,
            })
        );
        // `Esc` closes it, and the key map is live again.
        assert_eq!(press(&mut a, KeyCode::Esc), Vec::new());
        assert!(a.overlay.is_none());
        assert_eq!(press(&mut a, KeyCode::Char('q')), vec![Effect::Quit]);
    }

    #[test]
    fn a_confirmation_swallows_every_other_key() {
        let mut a = loaded();
        goto(&mut a, Screen::Profiles);
        press(&mut a, KeyCode::Char('d'));
        assert!(matches!(a.overlay, Some(Overlay::Confirm { .. })));
        // Neither `q` nor an unbound key may escape the question.
        assert_eq!(typed(&mut a, "qz"), Vec::new());
        assert!(!a.is_quit());
        assert!(matches!(a.overlay, Some(Overlay::Confirm { .. })));
    }

    #[test]
    fn a_prompt_edits_at_the_caret_and_handles_multibyte_text() {
        let mut a = loaded();
        goto(&mut a, Screen::Profiles);
        press(&mut a, KeyCode::Char('/'));
        typed(&mut a, "東京");
        typed(&mut a, "x");
        assert_eq!(press(&mut a, KeyCode::Backspace), Vec::new());
        assert_eq!(press(&mut a, KeyCode::Left), Vec::new());
        typed(&mut a, "y");
        let Some(Overlay::Prompt { value, cursor, .. }) = &a.overlay else {
            panic!("the prompt should still be open");
        };
        assert_eq!(value, "東y京");
        assert_eq!(*cursor, 2, "the caret is counted in characters");
    }

    #[test]
    fn a_prompt_can_be_cleared_and_home_and_end_move_the_caret() {
        let mut a = loaded();
        goto(&mut a, Screen::Profiles);
        press(&mut a, KeyCode::Char('/'));
        typed(&mut a, "abc");
        assert_eq!(press(&mut a, KeyCode::Home), Vec::new());
        let Some(Overlay::Prompt { cursor, .. }) = &a.overlay else {
            panic!("prompt");
        };
        assert_eq!(*cursor, 0);
        let _ = a.on_event(Event::Key(KeyEvent::new(
            KeyCode::Char('u'),
            KeyModifiers::CONTROL,
        )));
        let Some(Overlay::Prompt { value, cursor, .. }) = &a.overlay else {
            panic!("prompt");
        };
        assert!(value.is_empty());
        assert_eq!(*cursor, 0);
    }

    // -- confirmation gating ------------------------------------------------

    #[test]
    fn every_destructive_action_asks_first_and_only_runs_on_a_yes() {
        for action in [
            Action::DeleteProfile,
            Action::CloseAllConnections,
            Action::StopCore,
            Action::RollbackConfig,
            Action::UpgradeCore,
        ] {
            let mut a = loaded();
            goto(&mut a, Screen::Profiles);
            assert_eq!(a.dispatch(action.clone(), false), Vec::new());
            match &a.overlay {
                Some(Overlay::Confirm {
                    question,
                    action: held,
                }) => {
                    assert!(!question.is_empty());
                    assert_eq!(held, &action);
                }
                other => panic!("{action:?} should have asked, got {other:?}"),
            }
            // "no" leaves nothing behind.
            let _ = typed(&mut a, "n");
            assert!(a.overlay.is_none());
            // "yes" dispatches exactly once.
            let _ = a.dispatch(action.clone(), false);
            let effects = typed(&mut a, "y");
            assert_eq!(effects.len(), 1, "{action:?} should run once");
            assert!(a.overlay.is_none());
        }
    }

    #[test]
    fn a_destructive_action_that_cannot_do_anything_says_so_instead_of_asking() {
        let mut a = app();
        // Nothing is running, so there is nothing to stop.
        assert!(a.dispatch(Action::StopCore, false).is_empty());
        assert!(a.overlay.is_none(), "no pointless confirmation");
        assert_eq!(a.current_status().unwrap().kind, StatusKind::Warning);
    }

    #[test]
    fn delete_profile_really_deletes_the_highlighted_one() {
        let mut a = loaded();
        goto(&mut a, Screen::Profiles);
        a.profiles
            .select_by_key("patch".to_owned(), |r| r.uid.clone());
        let _ = press(&mut a, KeyCode::Char('d'));
        assert_eq!(
            typed(&mut a, "y"),
            vec![Effect::DeleteProfile {
                uid: "patch".to_owned()
            }]
        );
    }

    // -- filtering ----------------------------------------------------------

    #[test]
    fn the_search_prompt_filters_as_it_is_typed_and_esc_clears_it() {
        let mut a = loaded();
        goto(&mut a, Screen::Proxies);
        a.expanded.push("PROXY".to_owned());
        let _ = a.on_event(Event::Data(Data::Nodes(vec![
            node("PROXY", None, true, None),
            node("JP 01", Some("PROXY"), false, Some(20)),
            node("US 01", Some("PROXY"), false, Some(300)),
        ])));
        assert_eq!(press(&mut a, KeyCode::Char('/')), Vec::new());
        typed(&mut a, "jp");
        assert_eq!(a.nodes.len(), 1, "typing narrows the list immediately");
        assert_eq!(a.nodes.selected_item().unwrap().name, "JP 01");
        assert_eq!(press(&mut a, KeyCode::Esc), Vec::new());
        assert_eq!(a.nodes.len(), 3, "Esc restores the full list");
        assert_eq!(a.nodes.filter(), "");
    }

    #[test]
    fn search_next_walks_the_matches_and_wraps() {
        let mut a = app();
        let _ = a.on_event(Event::Data(Data::Nodes(vec![
            node("alpha", None, false, None),
            node("beta", None, false, None),
            node("bravo", None, false, None),
        ])));
        goto(&mut a, Screen::Proxies);
        a.nodes.set_filter("b");
        assert_eq!(a.nodes.len(), 2);
        a.nodes.select_first();
        assert_eq!(press(&mut a, KeyCode::Char('n')), Vec::new());
        assert_eq!(a.nodes.selected_item().unwrap().name, "bravo");
        assert_eq!(press(&mut a, KeyCode::Char('n')), Vec::new());
        assert_eq!(
            a.nodes.selected_item().unwrap().name,
            "beta",
            "search next wraps around"
        );
    }

    #[test]
    fn escape_without_a_filter_dismisses_the_footer() {
        let mut a = loaded();
        goto(&mut a, Screen::Rules);
        let _ = a.on_event(Event::Failed("a useful error".to_owned()));
        assert_eq!(press(&mut a, KeyCode::Esc), Vec::new());
        assert!(a.current_status().is_none());
        assert_eq!(a.last_status().unwrap().text, "a useful error");
    }

    // -- status -------------------------------------------------------------

    #[test]
    fn every_footer_message_expires() {
        let info = Status::new(StatusKind::Info, "done");
        assert!(!info.is_expired_at(info.at));
        assert!(!info.is_expired_at(info.at + STATUS_TTL.saturating_sub(Duration::from_millis(1))));
        assert!(info.is_expired_at(info.at + STATUS_TTL));

        let error = Status::new(StatusKind::Error, "boom");
        assert!(error.is_expired_at(error.at + STATUS_TTL));
    }

    #[test]
    fn a_message_disappears_after_its_lifetime_and_a_failure_replaces_it() {
        let mut a = loaded();
        let _ = a.on_event(Event::Done(Done::ChainSaved));
        let status = a.current_status().unwrap().clone();
        assert_eq!(status.kind, StatusKind::Success);

        let later = status.at + STATUS_TTL;
        a.expire_status_at(later);
        assert!(a.current_status().is_none(), "a success message expires");

        let _ = a.on_event(Event::Failed("the controller refused".to_owned()));
        let failed = a.current_status().unwrap();
        assert_eq!(failed.kind, StatusKind::Error);
        assert!(failed.text.contains("refused"));
        a.expire_status_at(failed.at + STATUS_TTL);
        assert!(a.current_status().is_none());
        assert_eq!(a.last_status().unwrap().text, "the controller refused");
    }

    #[test]
    fn a_failed_batch_leaves_no_test_marked_as_running() {
        let mut a = loaded();
        goto(&mut a, Screen::Tests);
        let _ = press(&mut a, KeyCode::Enter);
        assert_eq!(a.queued_tests(), 1);
        let _ = a.on_event(Event::Failed("core went away".to_owned()));
        assert_eq!(a.queued_tests(), 0);
        assert!(
            a.tests
                .items()
                .iter()
                .all(|row| row.result == TestResult::Pending)
        );
    }

    // -- selection preservation ---------------------------------------------

    #[test]
    fn a_refresh_keeps_the_selected_profile_by_name() {
        let mut a = loaded();
        goto(&mut a, Screen::Profiles);
        a.profiles
            .select_by_key("patch".to_owned(), |r| r.uid.clone());

        // A new subscription arrives at the top of the list.
        let _ = a.on_event(Event::Data(Data::Profiles(vec![
            profile("new", "new", Some("https://new.example/sub")),
            ProfileRow::from_item(&PrfItem::local("base", "base"), true, false),
            patch("patch", "merge", ProfileType::Merge),
        ])));
        assert_eq!(
            a.profiles.selected_item().unwrap().uid,
            "patch",
            "the cursor follows the row, not the position"
        );
    }

    #[test]
    fn a_refresh_keeps_the_selected_node_and_its_expansion() {
        let mut a = loaded();
        goto(&mut a, Screen::Proxies);
        a.expanded.push("PROXY".to_owned());
        let _ = a.on_event(Event::Data(Data::Nodes(vec![
            node("PROXY", None, true, None),
            node("JP 01", Some("PROXY"), false, Some(20)),
            node("US 01", Some("PROXY"), false, Some(300)),
        ])));
        a.nodes
            .select_by_key("US 01".to_owned(), |r| r.name.clone());
        // The same tree arrives again with a fresh latency reading.
        let _ = a.on_event(Event::Data(Data::Nodes(vec![
            node("PROXY", None, true, None),
            node("JP 01", Some("PROXY"), false, Some(15)),
            node("US 01", Some("PROXY"), false, Some(280)),
        ])));
        assert_eq!(a.nodes.selected_item().unwrap().name, "US 01");
        assert_eq!(a.nodes.selected_item().unwrap().delay, Some(280));
        assert_eq!(a.nodes.len(), 3, "the group stays expanded");
    }

    #[test]
    fn a_refresh_keeps_the_selected_connection_by_id() {
        let mut a = loaded();
        goto(&mut a, Screen::Connections);
        a.connections
            .select_by_key("c2".to_owned(), |r| r.id.clone());
        let _ = a.on_event(Event::Data(Data::Connections(vec![connection(
            "c0",
            "new.test:443",
            1,
        )])));
        // The selected connection is gone, so the cursor must simply stay valid.
        assert!(a.connections.selected_item().is_some());
        let _ = a.on_event(Event::Data(Data::Connections(vec![
            connection("c2", "b.test:443", 9999),
            connection("c3", "c.test:443", 1),
        ])));
        assert_eq!(a.connections.selected_item().unwrap().id, "c2");
    }

    // -- profiles -----------------------------------------------------------

    #[test]
    fn a_base_profile_cannot_be_chained_and_says_why() {
        let mut a = loaded();
        goto(&mut a, Screen::Profiles);
        a.profiles
            .select_by_key("base".to_owned(), |r| r.uid.clone());
        assert_eq!(press(&mut a, KeyCode::Char('c')), Vec::new());
        let warning = a.current_status().unwrap();
        assert_eq!(warning.kind, StatusKind::Warning);
        assert!(warning.text.contains("base document"), "{}", warning.text);
        assert!(a.chain().is_empty());
    }

    #[test]
    fn chaining_a_patch_toggles_it_and_saves_the_chain() {
        let mut a = loaded();
        goto(&mut a, Screen::Profiles);
        a.profiles
            .select_by_key("patch".to_owned(), |r| r.uid.clone());
        assert_eq!(
            press(&mut a, KeyCode::Char('c')),
            vec![Effect::SetChain {
                uids: vec!["patch".to_owned()]
            }]
        );
        assert_eq!(a.chain(), ["patch"]);
        assert!(
            a.profiles
                .items()
                .iter()
                .find(|r| r.uid == "patch")
                .unwrap()
                .in_chain
        );
        // Pressing it again removes the patch from the chain.
        assert_eq!(
            press(&mut a, KeyCode::Char('c')),
            vec![Effect::SetChain { uids: Vec::new() }]
        );
        assert!(a.chain().is_empty());
    }

    #[test]
    fn a_local_profile_has_nothing_to_download() {
        let mut a = loaded();
        goto(&mut a, Screen::Profiles);
        a.profiles
            .select_by_key("base".to_owned(), |r| r.uid.clone());
        assert_eq!(press(&mut a, KeyCode::Char('u')), Vec::new());
        assert!(
            a.current_status().unwrap().text.contains("local"),
            "the reason has to name the problem"
        );
    }

    #[test]
    fn update_all_delegates_due_selection_to_the_executor() {
        let mut a = loaded();
        goto(&mut a, Screen::Profiles);
        let effects = press(&mut a, KeyCode::Char('U'));
        assert_eq!(
            effects,
            vec![Effect::UpdateProfiles { uids: Vec::new() }],
            "the executor checks the full index for due profiles"
        );

        let _ = a.on_event(Event::Data(Data::Profiles(vec![
            ProfileRow::from_item(&PrfItem::local("base", "base"), true, false),
            profile("r1", "Tokyo", Some("https://sub.example/tokyo")),
            profile("r2", "Berlin", Some("https://sub.example/berlin")),
        ])));
        assert_eq!(
            press(&mut a, KeyCode::Char('U')),
            vec![Effect::UpdateProfiles { uids: Vec::new() }]
        );
    }

    #[test]
    fn the_new_profile_picker_leads_to_a_url_prompt() {
        let mut a = loaded();
        goto(&mut a, Screen::Profiles);
        assert_eq!(press(&mut a, KeyCode::Char('a')), Vec::new());
        assert!(matches!(a.overlay, Some(Overlay::Picker { .. })));
        // Enter picks "from a URL", which asks for the URL.
        assert_eq!(press(&mut a, KeyCode::Enter), Vec::new());
        assert!(matches!(
            a.overlay,
            Some(Overlay::Prompt {
                kind: PromptKind::Url,
                ..
            })
        ));
        typed(&mut a, "https://sub.example/tokyo");
        assert_eq!(
            press(&mut a, KeyCode::Enter),
            vec![Effect::NewProfile {
                name: "sub.example".to_owned(),
                url: Some("https://sub.example/tokyo".to_owned()),
            }]
        );
    }

    #[test]
    fn a_blank_local_profile_asks_for_a_name() {
        let mut a = loaded();
        goto(&mut a, Screen::Profiles);
        press(&mut a, KeyCode::Char('a'));
        let _ = press(&mut a, KeyCode::Down);
        let _ = press(&mut a, KeyCode::Enter);
        typed(&mut a, "office");
        assert_eq!(
            press(&mut a, KeyCode::Enter),
            vec![Effect::NewProfile {
                name: "office".to_owned(),
                url: None
            }]
        );
    }

    #[test]
    fn a_subscription_url_that_is_not_a_url_is_refused_and_the_prompt_stays() {
        let mut a = loaded();
        goto(&mut a, Screen::Profiles);
        press(&mut a, KeyCode::Char('a'));
        let _ = press(&mut a, KeyCode::Enter);
        typed(&mut a, "ftp://example.com/x");
        assert_eq!(press(&mut a, KeyCode::Enter), Vec::new());
        assert!(a.overlay.is_some(), "the prompt keeps what was typed");
        assert_eq!(a.current_status().unwrap().kind, StatusKind::Warning);
    }

    #[test]
    fn import_offers_the_homes_the_binary_found() {
        let mut a = loaded();
        goto(&mut a, Screen::Profiles);
        assert_eq!(
            press(&mut a, KeyCode::Char('i')),
            vec![Effect::DetectImportSources]
        );
        let _ = a.on_event(Event::Data(Data::ImportSources(vec![PathBuf::from(
            "/home/u/.config/clash-verge-rev",
        )])));
        assert!(matches!(a.overlay, Some(Overlay::Picker { .. })));
        assert_eq!(
            press(&mut a, KeyCode::Enter),
            vec![Effect::ImportProfiles {
                source: PathBuf::from("/home/u/.config/clash-verge-rev")
            }]
        );
    }

    #[test]
    fn a_missing_import_source_is_reported_rather_than_shown_as_an_empty_list() {
        let mut a = loaded();
        let _ = a.on_event(Event::Data(Data::ImportSources(Vec::new())));
        assert!(a.overlay.is_none());
        assert!(
            a.current_status()
                .unwrap()
                .text
                .contains("no clash-verge-rev"),
        );
    }

    #[test]
    fn renaming_offers_the_current_name_and_refuses_an_empty_one() {
        let mut a = loaded();
        goto(&mut a, Screen::Profiles);
        a.profiles
            .select_by_key("patch".to_owned(), |r| r.uid.clone());
        press(&mut a, KeyCode::Char('R'));
        let Some(Overlay::Prompt { value, .. }) = &a.overlay else {
            panic!("a rename prompt");
        };
        assert_eq!(value, "merge", "the prompt starts from the current name");

        // Clearing it and confirming is refused.
        let _ = a.on_event(Event::Key(KeyEvent::new(
            KeyCode::Char('u'),
            KeyModifiers::CONTROL,
        )));
        assert_eq!(press(&mut a, KeyCode::Enter), Vec::new());
        assert_eq!(a.current_status().unwrap().kind, StatusKind::Warning);
    }

    #[test]
    fn the_apply_mode_follows_the_settings() {
        let mut a = loaded();
        a.screen = Screen::Profiles;
        assert_eq!(
            a.dispatch(Action::ApplyConfig, false),
            vec![Effect::ApplyConfig {
                mode: ReloadMode::Auto
            }]
        );
        a.settings.update.prefer_hot_reload = false;
        assert_eq!(
            a.dispatch(Action::ApplyConfig, false),
            vec![Effect::ApplyConfig {
                mode: ReloadMode::Restart
            }],
            "a user who turned hot reload off gets a restart"
        );
    }

    #[test]
    fn a_preview_arrives_as_a_scrollable_overlay() {
        let mut a = loaded();
        let _ = a.on_event(Event::Data(Data::Preview(Box::new(Preview {
            summary: "12 proxies, 3 groups, 40 rules - no errors".to_owned(),
            changes: vec![PreviewChange {
                verb: '+',
                path: "proxies[name=JP 02]".to_owned(),
            }],
            findings: vec![PreviewFinding {
                tag: 'W',
                message: "group has no health check".to_owned(),
                location: Some("proxy-groups[0]".to_owned()),
                hint: Some("add url-test".to_owned()),
            }],
            warnings: vec!["a script profile was skipped".to_owned()],
            applicable: true,
            truncated: false,
        }))));
        let Some(Overlay::Preview { lines, scroll, .. }) = &a.overlay else {
            panic!("the preview should be on top");
        };
        assert_eq!(*scroll, 0);
        assert!(lines.iter().any(|l| l.contains("JP 02")));
        assert!(lines.iter().any(|l| l.contains("add url-test")));
        // It scrolls, and Enter closes it.
        let _ = press(&mut a, KeyCode::Down);
        let Some(Overlay::Preview { scroll, .. }) = &a.overlay else {
            panic!("still open");
        };
        assert_eq!(*scroll, 1);
        assert_eq!(press(&mut a, KeyCode::Enter), Vec::new());
        assert!(a.overlay.is_none());
    }

    // -- proxies ------------------------------------------------------------

    #[test]
    fn switching_a_profile_replaces_stale_proxy_data_and_prepares_or_applies() {
        let mut stopped = loaded();
        stopped.core = CoreStatus::Stopped;
        let effects = stopped.on_event(Event::Done(Done::ProfileSwitched {
            name: "new".to_owned(),
        }));
        assert!(stopped.nodes.is_empty());
        assert!(effects.contains(&Effect::PrepareConfig));
        assert!(effects.contains(&Effect::LoadProfiles));

        let mut running = loaded();
        let effects = running.on_event(Event::Done(Done::ProfileSwitched {
            name: "new".to_owned(),
        }));
        assert!(running.nodes.is_empty());
        assert!(effects.contains(&Effect::ApplyConfig {
            mode: ReloadMode::Auto,
        }));
    }

    #[test]
    fn deleting_the_current_profile_clears_proxies_and_stops_its_core() {
        let mut a = loaded();
        let effects = a.on_event(Event::Done(Done::ProfileDeleted {
            name: "base".to_owned(),
            was_current: true,
        }));
        assert!(a.nodes.is_empty());
        assert!(effects.contains(&Effect::StopCore));
    }

    #[test]
    fn routing_mode_cycles_through_all_three_core_modes() {
        let mut a = loaded();
        goto(&mut a, Screen::Home);
        a.core_mode = Some("rule".to_owned());
        assert_eq!(
            press(&mut a, KeyCode::Char('M')),
            vec![Effect::SetCoreMode {
                mode: "global".to_owned(),
            }]
        );
        let _ = a.on_event(Event::Done(Done::CoreModeChanged {
            mode: "global".to_owned(),
        }));
        assert_eq!(
            press(&mut a, KeyCode::Char('M')),
            vec![Effect::SetCoreMode {
                mode: "direct".to_owned(),
            }]
        );
        let _ = a.on_event(Event::Done(Done::CoreModeChanged {
            mode: "direct".to_owned(),
        }));
        assert_eq!(
            press(&mut a, KeyCode::Char('M')),
            vec![Effect::SetCoreMode {
                mode: "rule".to_owned(),
            }]
        );
    }

    #[test]
    fn a_group_row_expands_and_collapses_its_members() {
        let mut a = loaded();
        goto(&mut a, Screen::Proxies);
        assert_eq!(a.nodes.len(), 1, "groups start collapsed");
        assert_eq!(press(&mut a, KeyCode::Enter), Vec::new());
        assert_eq!(a.nodes.len(), 3);
        assert!(a.is_expanded("PROXY"));
        assert_eq!(press(&mut a, KeyCode::Enter), Vec::new());
        assert_eq!(a.nodes.len(), 1, "pressing again collapses it");
        assert!(!a.is_expanded("PROXY"));
    }

    #[test]
    fn a_member_row_is_pinned_in_its_group() {
        let mut a = loaded();
        goto(&mut a, Screen::Proxies);
        press(&mut a, KeyCode::Enter); // expand
        let _ = press(&mut a, KeyCode::Down); // onto JP 01
        assert_eq!(
            a.nodes.selected_item().unwrap().name,
            "JP 01",
            "the member is highlighted"
        );
        assert_eq!(
            press(&mut a, KeyCode::Enter),
            vec![Effect::SelectNode {
                group: "PROXY".to_owned(),
                member: "JP 01".to_owned()
            }]
        );
    }

    #[test]
    fn successful_node_selection_updates_the_visible_row_and_refreshes() {
        let mut a = loaded();
        goto(&mut a, Screen::Proxies);
        press(&mut a, KeyCode::Enter);
        let effects = a.on_event(Event::Done(Done::NodeSelected {
            group: "PROXY".to_owned(),
            member: "US 01".to_owned(),
        }));
        assert_eq!(effects, vec![Effect::Refresh(Screen::Proxies)]);
        assert!(
            a.nodes
                .items()
                .iter()
                .any(|row| row.name == "US 01" && row.active)
        );
        assert!(
            a.nodes
                .items()
                .iter()
                .any(|row| row.name == "JP 01" && !row.active)
        );
    }

    #[test]
    fn testing_reads_the_highlighted_row_the_way_the_proxies_screen_shows_it() {
        let mut a = loaded();
        goto(&mut a, Screen::Proxies);
        // A group row: `t` tests the group, `T` tests the group it belongs to.
        assert_eq!(
            press(&mut a, KeyCode::Char('t')),
            vec![Effect::TestGroup {
                group: "PROXY".to_owned()
            }]
        );
        press(&mut a, KeyCode::Enter);
        let _ = press(&mut a, KeyCode::Down);
        assert_eq!(
            press(&mut a, KeyCode::Char('t')),
            vec![Effect::TestNode {
                name: "JP 01".to_owned()
            }]
        );
        assert_eq!(
            press(&mut a, KeyCode::Char('T')),
            vec![Effect::TestGroup {
                group: "PROXY".to_owned()
            }]
        );
        assert_eq!(
            press(&mut a, KeyCode::Char('a')),
            vec![Effect::TestAllNodes]
        );
    }

    #[test]
    fn clearing_a_pin_names_the_group_that_owns_the_row() {
        let mut a = loaded();
        goto(&mut a, Screen::Proxies);
        assert_eq!(
            press(&mut a, KeyCode::Char('x')),
            vec![Effect::ClearNodePin {
                group: "PROXY".to_owned()
            }]
        );
        press(&mut a, KeyCode::Enter);
        let _ = press(&mut a, KeyCode::Down);
        assert_eq!(
            press(&mut a, KeyCode::Char('x')),
            vec![Effect::ClearNodePin {
                group: "PROXY".to_owned()
            }]
        );
    }

    #[test]
    fn a_core_action_needs_a_running_core_and_says_so() {
        let mut a = app();
        let _ = a.on_event(Event::Data(Data::Nodes(vec![
            node("PROXY", None, true, None),
            node("JP 01", Some("PROXY"), false, Some(20)),
        ])));
        goto(&mut a, Screen::Proxies);
        press(&mut a, KeyCode::Enter);
        let _ = press(&mut a, KeyCode::Down);
        assert_eq!(press(&mut a, KeyCode::Enter), Vec::new());
        let warning = a.current_status().unwrap();
        assert_eq!(warning.kind, StatusKind::Warning);
        assert!(warning.text.contains("running core"), "{}", warning.text);
    }

    // -- connections --------------------------------------------------------

    #[test]
    fn closing_a_connection_names_it_and_closing_all_asks_first() {
        let mut a = loaded();
        goto(&mut a, Screen::Connections);
        assert_eq!(
            press(&mut a, KeyCode::Char('d')),
            vec![Effect::CloseConnection {
                id: "c1".to_owned()
            }]
        );
        assert_eq!(press(&mut a, KeyCode::Char('D')), Vec::new());
        assert!(matches!(a.overlay, Some(Overlay::Confirm { .. })));
        assert_eq!(
            typed(&mut a, "y"),
            vec![Effect::CloseAllConnections],
            "an explicit yes dispatches the effect"
        );
    }

    #[test]
    fn cycling_the_sort_order_is_local_and_reorders_the_rows() {
        let mut a = loaded();
        goto(&mut a, Screen::Connections);
        assert_eq!(press(&mut a, KeyCode::Char('s')), Vec::new());
        assert_eq!(a.connection_sort, ConnectionSort::Busiest);
        assert_eq!(a.connections.items()[0].id, "c2", "the busiest comes first");
        assert!(a.current_status().unwrap().text.contains("busiest"));
        assert_eq!(press(&mut a, KeyCode::Char('s')), Vec::new());
        assert_eq!(a.connection_sort, ConnectionSort::Oldest);
    }

    // -- logs ---------------------------------------------------------------

    #[test]
    fn the_log_screen_controls_the_level_follow_and_export() {
        let mut a = loaded();
        goto(&mut a, Screen::Logs);
        assert_eq!(
            press(&mut a, KeyCode::Char('l')),
            vec![Effect::SetCoreLogLevel(LogLevel::Debug)],
            "the levels cycle quietest last: Info is followed by Debug"
        );
        assert_eq!(a.log_level, LogLevel::Debug);
        assert!(a.settings_dirty, "the level is part of the settings");

        let _ = press(&mut a, KeyCode::Char('f'));
        assert!(!a.logs.follow);
        let _ = press(&mut a, KeyCode::Char('f'));
        assert!(a.logs.follow);

        assert_eq!(press(&mut a, KeyCode::Char('x')), Vec::new());
        assert!(a.current_status().unwrap().text.contains("empty"));

        for i in 0..3 {
            let _ = a.on_event(Event::Data(Data::Log(LogRow::new(
                "info",
                format!("line {i}"),
            ))));
        }
        let effects = press(&mut a, KeyCode::Char('x'));
        match effects.as_slice() {
            [Effect::ExportLogs { path, contents }] => {
                assert!(path.starts_with(&a.home));
                assert!(path.to_string_lossy().contains("logs"));
                assert_eq!(contents.lines().count(), 3);
            }
            other => panic!("expected an export, got {other:?}"),
        }
        let _ = press(&mut a, KeyCode::Char('c'));
        assert!(a.logs.is_empty());
    }

    #[test]
    fn the_log_level_is_not_sent_to_a_core_that_is_not_running() {
        let mut a = app();
        goto(&mut a, Screen::Logs);
        assert_eq!(press(&mut a, KeyCode::Char('l')), Vec::new());
        assert!(a.current_status().unwrap().text.contains("not running"));
    }

    // -- rules --------------------------------------------------------------

    #[test]
    fn a_rule_is_toggled_the_other_way_and_disabled_rules_can_be_hidden() {
        let mut a = loaded();
        goto(&mut a, Screen::Rules);
        assert_eq!(a.rules.len(), 1, "the disabled rule is hidden by default");
        assert_eq!(
            press(&mut a, KeyCode::Enter),
            vec![Effect::ToggleRule {
                index: 0,
                disabled: true
            }]
        );
        assert_eq!(press(&mut a, KeyCode::Char('h')), Vec::new());
        assert!(a.show_disabled_rules);
        assert_eq!(a.rules.len(), 2);
        assert_eq!(
            press(&mut a, KeyCode::Char(' ')),
            vec![Effect::ToggleRule {
                index: 0,
                disabled: true
            }]
        );
    }

    #[test]
    fn updating_one_rule_set_falls_back_to_every_set_when_the_mapping_is_unknown() {
        let mut a = loaded();
        goto(&mut a, Screen::Rules);
        assert_eq!(
            press(&mut a, KeyCode::Char('u')),
            vec![Effect::UpdateRuleProviders { names: Vec::new() }],
            "a rule row carries no provider, so the request has to widen"
        );
        assert!(a.current_status().unwrap().text.contains("every set"));
        assert_eq!(
            press(&mut a, KeyCode::Char('U')),
            vec![Effect::UpdateRuleProviders { names: Vec::new() }]
        );

        // Once the core names its providers, a single one can be chosen.
        let _ = a.on_event(Event::Data(Data::RuleProviders(vec!["geosite".to_owned()])));
        assert_eq!(
            press(&mut a, KeyCode::Char('u')),
            vec![Effect::UpdateRuleProviders {
                names: vec!["geosite".to_owned()]
            }]
        );
    }

    // -- tests screen -------------------------------------------------------

    #[test]
    fn the_test_queue_runs_one_at_a_time_and_cancels_as_a_batch() {
        let mut a = loaded();
        goto(&mut a, Screen::Tests);
        assert_eq!(a.tests.len(), 4, "one row per kind of check");

        let first = press(&mut a, KeyCode::Enter);
        assert_eq!(a.queued_tests(), 1);
        assert!(matches!(a.tests.items()[0].result, TestResult::Running));
        let target = a.tests.items()[0].target.clone();
        assert_eq!(
            first,
            vec![Effect::RunTest {
                kind: TestKind::GroupLatency,
                target
            }]
        );

        // Queueing a second test does not start it early.
        let _ = press(&mut a, KeyCode::Down);
        assert_eq!(press(&mut a, KeyCode::Enter), Vec::new());
        assert_eq!(a.queued_tests(), 2);
        assert!(a.current_status().unwrap().text.contains("queued"));

        // The answer to the first starts the second.
        let effects = a.on_event(Event::Data(Data::TestResult {
            kind: TestKind::GroupLatency,
            target: a.tests.items()[0].target.clone(),
            result: TestResult::Passed("42 ms".to_owned()),
        }));
        assert_eq!(a.queued_tests(), 1);
        assert!(matches!(effects.as_slice(), [Effect::RunTest { .. }]));

        assert_eq!(press(&mut a, KeyCode::Char('s')), vec![Effect::CancelTests]);
        assert_eq!(a.queued_tests(), 0);
        assert_eq!(a.current_status().unwrap().kind, StatusKind::Warning);
    }

    #[test]
    fn a_test_that_has_run_is_not_queued_twice() {
        let mut a = loaded();
        goto(&mut a, Screen::Tests);
        let _ = press(&mut a, KeyCode::Enter);
        let _ = a.on_event(Event::Data(Data::TestResult {
            kind: TestKind::GroupLatency,
            target: a.tests.items()[0].target.clone(),
            result: TestResult::Passed("12 ms".to_owned()),
        }));
        assert_eq!(press(&mut a, KeyCode::Enter), Vec::new());
        assert!(a.current_status().unwrap().text.contains("already run"));
        assert_eq!(
            press(&mut a, KeyCode::Char('c')),
            vec![Effect::ClearTestResults],
            "clearing puts it back to pending"
        );
        assert!(
            a.tests
                .items()
                .iter()
                .all(|r| r.result == TestResult::Pending)
        );
    }

    #[test]
    fn a_finished_batch_orders_the_nodes_by_latency() {
        let mut a = loaded();
        goto(&mut a, Screen::Proxies);
        press(&mut a, KeyCode::Enter);
        let _ = a.on_event(Event::Done(Done::NodeTestsFinished { tested: 2 }));
        assert_eq!(a.node_sort, SortOrder::LatencyAscending);
        let names: Vec<&str> = a.nodes.items().iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["PROXY", "JP 01", "US 01"],
            "the group stays on top and its members are ordered fastest first"
        );
    }

    // -- settings -----------------------------------------------------------

    /// A typed-in number is held to the same ceiling a save is.
    ///
    /// The class the ninth review found for the command line, one crate over:
    /// `--timeout 32768` was refused by the settings and accepted by the flag.
    /// Here the flag's equivalent is the prompt, and it accepted any parseable
    /// number — which then sat in memory until a save failed.
    #[test]
    fn a_typed_in_number_the_settings_refuse_is_refused_at_the_prompt() {
        let mut settings = Settings::default();
        let before = settings.test.timeout_ms;

        let refused = set_setting_text(&mut settings, "test.timeout_ms", "99999");
        assert!(refused.is_err(), "the core parses this as an int16");
        assert_eq!(
            settings.test.timeout_ms, before,
            "and the value is rolled back rather than kept"
        );

        // The ceiling itself is accepted, so the check is the settings' and not
        // a second, stricter one written here.
        assert!(
            set_setting_text(
                &mut settings,
                "test.timeout_ms",
                &cvt_core::settings::MAX_TEST_TIMEOUT_MS.to_string()
            )
            .is_ok()
        );

        // And the same for the other ceiling, and for a value that is not a
        // number at all.
        assert!(set_setting_text(&mut settings, "test.concurrency", "0").is_err());
        assert!(set_setting_text(&mut settings, "logs.keep", "65").is_err());
        assert!(set_setting_text(&mut settings, "test.timeout_ms", "soon").is_err());
    }

    #[test]
    fn every_setting_row_can_be_cycled_from_the_keyboard() {
        let mut a = loaded();
        goto(&mut a, Screen::Settings);
        assert_eq!(a.settings_rows.len(), 25);
        a.settings_rows
            .select_by_key("ui.color".to_owned(), |row| row.key.to_owned());
        assert_eq!(press(&mut a, KeyCode::Enter), Vec::new());
        assert!(!a.settings.ui.color);
        assert!(a.settings_dirty);
        assert!(!a.theme.color, "the theme follows the setting");
        assert!(
            a.current_status().unwrap().text.contains("colour"),
            "the status names the row that changed"
        );

        a.settings_rows
            .select_by_key("core.use_managed".to_owned(), |row| row.key.to_owned());
        let _ = press(&mut a, KeyCode::Enter);
        assert!(!a.settings.core.use_managed);

        a.settings_rows
            .select_by_key("test.concurrency".to_owned(), |row| row.key.to_owned());
        let _ = press(&mut a, KeyCode::Enter);
        assert_eq!(a.settings.test.concurrency, 32, "16 steps up to 32");
        let _ = press(&mut a, KeyCode::Char(' '));
        assert_eq!(a.settings.test.concurrency, 64);
        let _ = press(&mut a, KeyCode::Char(' '));
        assert_eq!(a.settings.test.concurrency, 1, "the cycle wraps");
    }

    #[test]
    fn changing_language_rebuilds_settings_rows_immediately() {
        let mut app = loaded();
        goto(&mut app, Screen::Settings);
        app.settings_rows
            .select_by_key("ui.language".to_owned(), |row| row.key.to_owned());
        let _ = press(&mut app, KeyCode::Enter);
        assert_eq!(app.settings.ui.language, Language::Chinese);
        assert!(app.settings_dirty);
        let row = app.settings_rows.selected_item().expect("language row");
        assert_eq!(row.key, "ui.language");
        assert_eq!(row.label, "界面语言");
        assert_eq!(row.value, "简体中文");
        let _ = press(&mut app, KeyCode::Enter);
        assert_eq!(app.settings.ui.language, Language::English);
        assert_eq!(app.settings_rows.selected_item().unwrap().label, "language");
    }

    #[test]
    fn a_text_setting_is_edited_in_a_prompt_and_validated() {
        let mut a = loaded();
        goto(&mut a, Screen::Settings);
        a.settings_rows
            .select_by_key("test.url".to_owned(), |row| row.key.to_owned());
        let _ = press(&mut a, KeyCode::Enter);
        let Some(Overlay::Prompt { label, value, .. }) = &a.overlay else {
            panic!("a prompt for the URL");
        };
        assert_eq!(label, "latency test URL");
        assert!(
            !value.is_empty(),
            "the prompt starts from the current value"
        );

        let _ = a.on_event(Event::Key(KeyEvent::new(
            KeyCode::Char('u'),
            KeyModifiers::CONTROL,
        )));
        typed(&mut a, "nonsense");
        assert_eq!(press(&mut a, KeyCode::Enter), Vec::new());
        assert!(
            a.overlay.is_some(),
            "an invalid value keeps the prompt open"
        );
        assert!(a.current_status().unwrap().text.contains("http"));

        let _ = a.on_event(Event::Key(KeyEvent::new(
            KeyCode::Char('u'),
            KeyModifiers::CONTROL,
        )));
        typed(&mut a, "https://example.com/generate_204");
        assert_eq!(press(&mut a, KeyCode::Enter), Vec::new());
        assert!(a.overlay.is_none());
        assert_eq!(a.settings.test.url, "https://example.com/generate_204");
        assert!(a.settings_dirty);
    }

    #[test]
    fn saving_the_settings_sends_them_and_clears_the_dirty_flag() {
        let mut a = loaded();
        goto(&mut a, Screen::Settings);
        a.settings_rows
            .select_by_key("ui.show_footer".to_owned(), |row| row.key.to_owned());
        let _ = press(&mut a, KeyCode::Enter);
        assert!(a.settings_dirty);
        let effects = press(&mut a, KeyCode::Char('s'));
        match effects.as_slice() {
            [Effect::SaveSettings { settings }] => assert!(!settings.ui.show_footer),
            other => panic!("expected a save, got {other:?}"),
        }
        let _ = a.on_event(Event::Done(Done::SettingsSaved));
        assert!(!a.settings_dirty);
    }

    #[test]
    fn the_settings_screen_keeps_its_place_when_a_value_changes() {
        let mut a = loaded();
        goto(&mut a, Screen::Settings);
        a.settings_rows
            .select_by_key("stream.logs".to_owned(), |row| row.key.to_owned());
        let _ = press(&mut a, KeyCode::Enter);
        assert_eq!(
            a.settings_rows.selected_item().unwrap().key,
            "stream.logs",
            "rebuilding the rows must not move the cursor"
        );
    }

    // -- core ---------------------------------------------------------------

    #[test]
    fn the_home_screen_carries_the_core_actions() {
        let mut a = loaded();
        goto(&mut a, Screen::Home);
        assert_eq!(press(&mut a, KeyCode::Char('R')), vec![Effect::RestartCore]);
        assert_eq!(press(&mut a, KeyCode::Char('g')), vec![Effect::UpdateGeo]);
        assert_eq!(press(&mut a, KeyCode::Char('F')), vec![Effect::FlushCaches]);
        assert_eq!(
            press(&mut a, KeyCode::Char('e')),
            vec![Effect::OpenEditor {
                path: a.home.join("runtime").join("config.yaml")
            }]
        );
        assert_eq!(press(&mut a, KeyCode::Char('S')), Vec::new());
        assert!(matches!(a.overlay, Some(Overlay::Confirm { .. })));
        assert_eq!(typed(&mut a, "y"), vec![Effect::StopCore]);
    }

    #[test]
    fn starting_the_core_does_not_ask_because_it_destroys_nothing() {
        let mut a = loaded();
        goto(&mut a, Screen::Home);
        assert_eq!(press(&mut a, KeyCode::Char('s')), vec![Effect::StartCore]);
        assert!(a.overlay.is_none());
    }

    // -- refresh and ticking ------------------------------------------------

    #[test]
    fn the_first_visit_to_a_screen_loads_it_and_later_ones_do_not() {
        let mut a = app();
        assert_eq!(
            press(&mut a, KeyCode::Char('2')),
            vec![Effect::LoadProfiles]
        );
        assert_eq!(
            press(&mut a, KeyCode::Char('1')),
            vec![Effect::Refresh(Screen::Home)]
        );
        assert_eq!(press(&mut a, KeyCode::Char('2')), Vec::new());
        assert_eq!(
            press(&mut a, KeyCode::Char('r')),
            vec![Effect::LoadProfiles],
            "an explicit refresh always asks"
        );
        assert_eq!(
            press(&mut a, KeyCode::Char('9')),
            Vec::new(),
            "the help screen has nothing to load"
        );
    }

    #[test]
    fn tabs_move_in_both_directions() {
        let mut a = app();
        assert_eq!(a.screen, Screen::Home);
        let _ = press(&mut a, KeyCode::Tab);
        assert_eq!(a.screen, Screen::Profiles);
        let _ = press(&mut a, KeyCode::BackTab);
        assert_eq!(a.screen, Screen::Home);
    }

    #[test]
    fn a_tick_expires_messages_and_polls_only_a_running_core() {
        let mut a = app();
        for _ in 0..POLL_EVERY {
            assert_eq!(a.on_tick(), Vec::new(), "a stopped core is not polled");
        }
        core_running(&mut a);
        let mut polls = Vec::new();
        for _ in 0..POLL_EVERY {
            polls.extend(a.on_tick());
        }
        assert_eq!(polls, vec![Effect::Refresh(Screen::Home)]);
    }

    #[test]
    fn resizing_updates_how_much_of_a_list_is_on_screen() {
        let mut a = loaded();
        goto(&mut a, Screen::Proxies);
        let _ = a.on_event(Event::Resize(120, 40));
        assert_eq!(a.viewport, (120, 40));
        assert_eq!(a.visible_rows(), 27);
        // A one-row terminal must still be usable rather than a panic.
        let _ = a.on_event(Event::Resize(1, 1));
        assert_eq!(a.visible_rows(), 1);
        assert_eq!(press(&mut a, KeyCode::PageDown), Vec::new());
    }

    #[test]
    fn mouse_selects_tabs_and_visible_rows_without_triggering_actions() {
        fn mouse(kind: MouseEventKind, column: u16, row: u16) -> Event {
            Event::Mouse(MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            })
        }
        let mut a = loaded();
        let _ = a.on_event(Event::Resize(120, 40));
        assert_eq!(
            a.on_event(mouse(MouseEventKind::Down(MouseButton::Left), 9, 0)),
            vec![Effect::LoadProfiles]
        );
        assert_eq!(a.screen, Screen::Profiles);
        let area = crate::ui::table_rows_area(&a).unwrap();
        assert!(
            a.on_event(mouse(
                MouseEventKind::Down(MouseButton::Left),
                area.x,
                area.y + 1
            ))
            .is_empty()
        );
        assert_eq!(a.profiles.selected_index(), 1);
        let _ = a.on_event(mouse(MouseEventKind::ScrollUp, area.x, area.y));
        assert_eq!(a.profiles.selected_index(), 0);
        let _ = a.on_event(mouse(
            MouseEventKind::Down(MouseButton::Left),
            area.x,
            area.y + 10,
        ));
        assert_eq!(
            a.profiles.selected_index(),
            0,
            "blank cells do not select a row"
        );

        a.overlay = Some(Overlay::Confirm {
            question: "delete?".to_owned(),
            action: Action::Quit,
        });
        let _ = a.on_event(mouse(MouseEventKind::Down(MouseButton::Left), 2, 0));
        assert_eq!(a.screen, Screen::Profiles, "a modal blocks tab clicks");
        assert!(a.overlay.is_some());
    }

    #[test]
    fn mouse_wheel_freezes_and_resumes_the_log_window() {
        let mut a = app();
        a.screen = Screen::Logs;
        for index in 0..8 {
            a.logs.push(LogRow {
                at: "now".to_owned(),
                level: "info".to_owned(),
                message: index.to_string(),
            });
        }
        let wheel = |kind| {
            Event::Mouse(MouseEvent {
                kind,
                column: 2,
                row: 3,
                modifiers: KeyModifiers::NONE,
            })
        };
        let _ = a.on_event(wheel(MouseEventKind::ScrollUp));
        assert!(!a.logs.follow);
        assert_eq!(a.log_window(2).1, 3);
        let _ = a.on_event(wheel(MouseEventKind::ScrollDown));
        assert!(a.logs.follow);
        assert_eq!(a.log_window(2).1, 0);
    }

    #[test]
    fn mouse_click_uses_the_scrolled_rows_and_the_modal_picker() {
        let mut a = app();
        let profiles = (0..40)
            .map(|index| {
                ProfileRow::from_item(
                    &PrfItem::local(format!("p{index}"), format!("profile {index}")),
                    false,
                    false,
                )
            })
            .collect();
        let _ = a.on_event(Event::Data(Data::Profiles(profiles)));
        a.screen = Screen::Profiles;
        let _ = a.on_event(Event::Resize(80, 20));
        a.profiles.select(20);
        a.sync_scroll();
        let offset = a.profiles.offset();
        assert!(offset > 0);
        let area = crate::ui::table_rows_area(&a).unwrap();
        let click = |column, row| {
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column,
                row,
                modifiers: KeyModifiers::NONE,
            })
        };
        let _ = a.on_event(click(area.x, area.y));
        assert_eq!(a.profiles.selected_index(), offset);

        a.overlay = Some(Overlay::Picker {
            title: "new profile".to_owned(),
            items: vec!["from a URL".to_owned(), "a blank local profile".to_owned()],
            selected: 0,
        });
        let picker_row = crate::ui::picker_item_at(a.viewport, 2, 0, 0, 0);
        assert!(picker_row.is_none(), "outside the popup has no choice");
        let popup = crate::ui::widgets::centered(ratatui::layout::Rect::new(0, 0, 80, 20), 72, 4);
        let _ = a.on_event(click(popup.x + 1, popup.y + 2));
        assert!(matches!(
            a.overlay,
            Some(Overlay::Prompt {
                kind: PromptKind::Name,
                ..
            })
        ));
    }

    #[test]
    fn mouse_buttons_confirm_cancel_and_accept_modal_input() {
        let mut a = app();
        a.viewport = (80, 24);
        let click = |column, row| {
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column,
                row,
                modifiers: KeyModifiers::NONE,
            })
        };

        a.overlay = Some(Overlay::Prompt {
            label: "profile name".to_owned(),
            kind: PromptKind::Name,
            value: "sample".to_owned(),
            cursor: 6,
        });
        assert_eq!(
            a.on_event(click(6, 12)),
            vec![Effect::NewProfile {
                name: "sample".to_owned(),
                url: None,
            }]
        );
        a.overlay = Some(Overlay::Prompt {
            label: "profile name".to_owned(),
            kind: PromptKind::Name,
            value: "discard".to_owned(),
            cursor: 7,
        });
        assert!(a.on_event(click(30, 12)).is_empty());
        assert!(a.overlay.is_none());

        a.overlay = Some(Overlay::Confirm {
            question: "quit?".to_owned(),
            action: Action::Quit,
        });
        assert_eq!(
            crate::ui::confirm_choice_at(&a, "quit?", 18, 13),
            Some(false)
        );
        assert!(a.on_event(click(18, 13)).is_empty());
        assert!(a.overlay.is_none());
        assert!(!a.is_quit());
        a.overlay = Some(Overlay::Confirm {
            question: "quit?".to_owned(),
            action: Action::Quit,
        });
        assert_eq!(a.on_event(click(6, 13)), vec![Effect::Quit]);
    }

    #[test]
    fn full_error_message_survives_footer_expiry_and_scrolls_in_its_popup() {
        let mut a = app();
        a.viewport = (40, 10);
        let message = format!("{}\n{}", "first ".repeat(30), "last detail");
        let _ = a.on_event(Event::Failed(message.clone()));
        let at = a.current_status().unwrap().at;
        a.expire_status_at(at + STATUS_TTL);
        assert!(a.current_status().is_none());
        let _ = a.show_last_message();
        assert!(matches!(&a.overlay, Some(Overlay::Message { text, .. }) if text == &message));
        let _ = a.on_event(Event::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 10,
            row: 5,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(matches!(
            a.overlay,
            Some(Overlay::Message { scroll: 1.., .. })
        ));
        assert!(press(&mut a, KeyCode::Esc).is_empty());
        assert!(a.overlay.is_none());
    }

    // -- navigation ---------------------------------------------------------

    #[test]
    fn movement_clamps_and_pages_by_a_screenful() {
        let mut a = loaded();
        goto(&mut a, Screen::Proxies);
        a.expanded.push("PROXY".to_owned());
        let _ = a.on_event(Event::Data(Data::Nodes(vec![
            node("PROXY", None, true, None),
            node("JP 01", Some("PROXY"), false, Some(20)),
            node("US 01", Some("PROXY"), false, Some(300)),
        ])));
        let _ = a.on_event(Event::Resize(80, 10));
        assert_eq!(a.nodes.selected_item().unwrap().name, "PROXY");
        let _ = press(&mut a, KeyCode::Char('k'));
        assert_eq!(a.nodes.selected_item().unwrap().name, "PROXY");
        let _ = press(&mut a, KeyCode::Char('G'));
        assert_eq!(a.nodes.selected_item().unwrap().name, "US 01");
        let _ = press(&mut a, KeyCode::Char('g'));
        assert_eq!(a.nodes.selected_item().unwrap().name, "PROXY");
        let _ = press(&mut a, KeyCode::Char('j'));
        let _ = press(&mut a, KeyCode::Char('j'));
        assert_eq!(a.nodes.selected_item().unwrap().name, "US 01");
    }

    #[test]
    fn an_empty_list_absorbs_navigation_without_panicking() {
        let mut a = app();
        for screen in [
            Screen::Profiles,
            Screen::Proxies,
            Screen::Connections,
            Screen::Rules,
            Screen::Tests,
            Screen::Settings,
        ] {
            a.screen = screen;
            let _ = press(&mut a, KeyCode::Down);
            let _ = press(&mut a, KeyCode::Up);
            let _ = press(&mut a, KeyCode::PageDown);
            let _ = press(&mut a, KeyCode::Char('G'));
            let _ = press(&mut a, KeyCode::Enter);
            let _ = press(&mut a, KeyCode::Char('x'));
        }
        // Nothing above should have produced an effect or a panic.
        assert!(!a.is_quit());
    }

    #[test]
    fn every_action_is_answered_with_an_effect_or_a_reason() {
        // The dispatcher must not have a silent arm: each action either does
        // something, asks a question, or explains why it cannot.
        for action in all_actions() {
            // Pure navigation is answered by the cursor moving rather than by
            // a message, and is covered by the movement tests; the contract
            // here is that every *other* action says or does something.
            if matches!(
                action,
                Action::Up
                    | Action::Down
                    | Action::PageUp
                    | Action::PageDown
                    | Action::Top
                    | Action::Bottom
                    | Action::SearchNext
            ) {
                let mut a = loaded();
                a.screen = Screen::Proxies;
                assert!(
                    a.dispatch(action.clone(), true).is_empty(),
                    "{action:?} must move the cursor, not fire an effect"
                );
                continue;
            }
            if action == Action::Cancel {
                let mut a = loaded();
                a.set_status(StatusKind::Error, "dismiss me");
                assert!(a.dispatch(action, true).is_empty());
                assert!(a.current_status().is_none());
                continue;
            }
            let mut a = loaded();
            let before = a.screen;
            let expanded_before = a.is_expanded("PROXY");
            let effects = a.dispatch(action.clone(), true);
            let answered = !effects.is_empty()
                || a.overlay.is_some()
                || a.current_status().is_some()
                || a.is_quit()
                || a.screen != before
                // Expanding a group is its own answer: it changes what the
                // proxies list shows without firing an effect.
                || a.is_expanded("PROXY") != expanded_before;
            assert!(answered, "{action:?} was answered with silence");
        }
    }

    /// Every action, so the check above cannot silently skip one.
    fn all_actions() -> Vec<Action> {
        let mut out = vec![
            Action::Quit,
            Action::NextScreen,
            Action::PreviousScreen,
            Action::Refresh,
            Action::Cancel,
            Action::ShowLastMessage,
            Action::Up,
            Action::Down,
            Action::PageUp,
            Action::PageDown,
            Action::Top,
            Action::Bottom,
            Action::Search,
            Action::SearchNext,
            Action::ActivateProfile,
            Action::UpdateProfile,
            Action::UpdateAllProfiles,
            Action::NewProfile,
            Action::DeleteProfile,
            Action::RenameProfile,
            Action::ImportProfiles,
            Action::EditProfile,
            Action::ToggleInChain,
            Action::PreviewConfig,
            Action::ApplyConfig,
            Action::RollbackConfig,
            Action::SelectNode,
            Action::TestGroup,
            Action::TestNode,
            Action::TestAllNodes,
            Action::ClearNodeSelection,
            Action::CloseConnection,
            Action::CloseAllConnections,
            Action::CycleConnectionSort,
            Action::ToggleLogFollow,
            Action::CycleLogLevel,
            Action::ClearLogs,
            Action::ExportLogs,
            Action::ToggleRule,
            Action::UpdateRuleProvider,
            Action::UpdateAllRuleProviders,
            Action::ToggleDisabledRules,
            Action::RunTests,
            Action::CancelTests,
            Action::ClearTestResults,
            Action::StartCore,
            Action::StopCore,
            Action::RestartCore,
            Action::CycleCoreMode,
            Action::UpgradeCore,
            Action::UpdateGeo,
            Action::FlushCaches,
            Action::EditRuntimeConfig,
            Action::SaveSettings,
            Action::ToggleSetting,
        ];
        out.extend(Screen::all().into_iter().map(Action::Goto));
        out
    }

    /// The action keys documented in the footer must be the ones that fire.
    #[test]
    fn a_representative_key_on_every_screen_produces_exactly_one_effect() {
        let cases: [(Screen, KeyCode, Action); 10] = [
            (Screen::Home, KeyCode::Char('s'), Action::StartCore),
            (Screen::Profiles, KeyCode::Char('e'), Action::EditProfile),
            (Screen::Profiles, KeyCode::Char('p'), Action::PreviewConfig),
            (Screen::Proxies, KeyCode::Char('T'), Action::TestGroup),
            (
                Screen::Connections,
                KeyCode::Char('d'),
                Action::CloseConnection,
            ),
            (Screen::Logs, KeyCode::Char('l'), Action::CycleLogLevel),
            (Screen::Rules, KeyCode::Enter, Action::ToggleRule),
            (Screen::Tests, KeyCode::Enter, Action::RunTests),
            (Screen::Settings, KeyCode::Char('s'), Action::SaveSettings),
            (Screen::Home, KeyCode::Char('r'), Action::Refresh),
        ];
        for (screen, code, action) in cases {
            let mut a = loaded();
            a.screen = screen;
            let direct = a.dispatch(action.clone(), true);
            let mut b = loaded();
            b.screen = screen;
            let by_key = b.on_event(Event::Key(key(code)));
            assert_eq!(
                direct, by_key,
                "{code:?} on {screen} should mean {action:?}"
            );
            assert!(!direct.is_empty());
        }
    }

    #[test]
    fn stopping_the_log_follow_freezes_the_window_without_losing_lines() {
        let mut a = loaded();
        goto(&mut a, Screen::Logs);
        for i in 0..5 {
            let _ = a.on_event(Event::Data(Data::Log(LogRow::new(
                "info",
                format!("line {i}"),
            ))));
        }
        let (window, hidden) = a.log_window(2);
        assert_eq!(hidden, 0);
        assert_eq!(
            window
                .iter()
                .map(|l| l.message.as_str())
                .collect::<Vec<_>>(),
            vec!["line 3", "line 4"],
            "the window shows the newest lines while following"
        );

        let _ = press(&mut a, KeyCode::Char('f'));
        for i in 5..9 {
            let _ = a.on_event(Event::Data(Data::Log(LogRow::new(
                "info",
                format!("line {i}"),
            ))));
        }
        let (frozen, hidden) = a.log_window(2);
        assert_eq!(
            frozen
                .iter()
                .map(|l| l.message.as_str())
                .collect::<Vec<_>>(),
            vec!["line 3", "line 4"],
            "a frozen view must not jump to the newest line"
        );
        assert_eq!(hidden, 4, "the hidden count is what the title reports");
        assert_eq!(a.logs.len(), 9, "nothing was discarded");

        let _ = press(&mut a, KeyCode::Char('f'));
        let (again, hidden) = a.log_window(1);
        assert_eq!(hidden, 0);
        assert_eq!(again[0].message, "line 8", "following resumes at the end");
    }

    #[test]
    fn clearing_the_logs_also_clears_the_frozen_window() {
        let mut a = loaded();
        goto(&mut a, Screen::Logs);
        let _ = a.on_event(Event::Data(Data::Log(LogRow::new("info", "one"))));
        let _ = press(&mut a, KeyCode::Char('f'));
        let _ = press(&mut a, KeyCode::Char('c'));
        assert!(a.logs.is_empty());
        assert_eq!(a.log_window(10).1, 0);
    }

    #[test]
    fn the_rule_count_reports_what_the_list_is_hiding() {
        let mut a = loaded();
        goto(&mut a, Screen::Rules);
        assert_eq!(a.rules.len(), 1);
        assert_eq!(a.hidden_rules(), 1);
        let _ = press(&mut a, KeyCode::Char('h'));
        assert_eq!(a.hidden_rules(), 0);
    }

    #[test]
    fn the_active_filter_is_readable_from_outside_the_dispatcher() {
        let mut a = loaded();
        goto(&mut a, Screen::Profiles);
        assert!(a.active_filter().is_empty());
        press(&mut a, KeyCode::Char('/'));
        typed(&mut a, "tok");
        assert_eq!(a.active_filter(), "tok");
        // The filter belongs to the screen it was typed on.
        a.screen = Screen::Rules;
        assert!(a.active_filter().is_empty());
    }

    #[test]
    fn a_settings_reload_replaces_the_theme_and_the_level() {
        let mut a = app();
        assert!(a.theme.color, "the constructed palette is used until then");
        let mut settings = Settings::default();
        settings.ui.color = false;
        settings.ui.log_level = LogLevel::Debug;
        let _ = a.on_event(Event::Data(Data::Settings(Box::new(settings))));
        assert!(!a.theme.color, "the colour setting rebuilds the palette");
        assert_eq!(a.log_level, LogLevel::Debug);
        assert!(!a.settings_dirty, "data from disk is not an unsaved edit");
    }

    #[test]
    fn a_note_reaches_the_status_line_without_an_overlay() {
        let mut a = loaded();
        let _ = a.on_event(Event::Data(Data::Notice("3 warnings".to_owned())));
        assert!(a.overlay.is_none());
        assert_eq!(a.current_status().unwrap().text, "3 warnings");
    }

    #[test]
    fn a_lost_core_stops_every_queued_test() {
        let mut a = loaded();
        goto(&mut a, Screen::Tests);
        let _ = press(&mut a, KeyCode::Enter);
        assert_eq!(a.queued_tests(), 1);
        let _ = a.on_event(Event::Data(Data::Core(CoreStatus::Stopped)));
        assert_eq!(a.queued_tests(), 0);
        assert!(
            a.tests
                .items()
                .iter()
                .all(|row| !matches!(row.result, TestResult::Running)),
            "a test cannot still be running once the core is gone"
        );
    }

    #[test]
    fn the_test_targets_follow_the_data_and_keep_the_cursor() {
        let mut a = loaded();
        goto(&mut a, Screen::Tests);
        assert_eq!(a.tests.items()[0].target, "PROXY");
        let _ = press(&mut a, KeyCode::Down);
        let _ = press(&mut a, KeyCode::Down);
        assert_eq!(a.tests.selected_item().unwrap().kind, TestKind::CoreHealth);

        // A refreshed proxy list lands while the screen is open.
        let _ = a.on_event(Event::Data(Data::Nodes(vec![node(
            "OFFICE", None, true, None,
        )])));
        assert_eq!(a.tests.items()[0].target, "OFFICE");
        assert_eq!(
            a.tests.selected_item().unwrap().kind,
            TestKind::CoreHealth,
            "a refresh must not move the cursor"
        );
    }

    #[test]
    fn the_editor_opens_the_runtime_configuration_the_core_was_launched_with() {
        let mut a = loaded();
        a.home = PathBuf::from("/srv/cvt");
        goto(&mut a, Screen::Home);
        assert_eq!(
            press(&mut a, KeyCode::Char('e')),
            vec![Effect::OpenEditor {
                path: PathBuf::from("/srv/cvt/runtime/config.yaml")
            }]
        );
    }
}
