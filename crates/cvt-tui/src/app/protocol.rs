//! Protocol responsibilities of the application state machine.

use crate::action::Screen;
use crate::row::{
    ConnectionRow, LogRow, NodeRow, ProbeMode, ProfileRow, RuleRow, TestKind, TestResult,
};
use crossterm::event::{KeyEvent, MouseEvent};
use cvt_core::ReloadMode;
use cvt_core::mihomo::supervisor::CoreStatus;
use cvt_core::mihomo::types::{LogLevel, Traffic};
use cvt_core::settings::Settings;
use std::path::PathBuf;

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
    /// Copy explicitly selected text without logging the payload.
    CopyText {
        /// Complete untruncated text.
        text: String,
    },
    /// Copy a local profile document.
    CopyProfile {
        /// Selected profile.
        uid: String,
    },
    /// Copy the effective outbound definition for a selected node.
    CopyProxy {
        /// Selected node.
        name: String,
    },
    /// Run the enabled launch actions once, after the terminal is ready.
    Startup,
    /// Query the latest stable application release.
    CheckAppUpdate {
        /// Ignore reminder suppression for an explicit user request.
        manual: bool,
    },
    /// Download, verify and replace the running application executable.
    InstallAppUpdate {
        /// The confirmed release tag, pinned throughout installation.
        tag: String,
    },
    /// Persist a postponed reminder or skipped version.
    DismissAppUpdate {
        /// Release being dismissed.
        tag: String,
        /// Permanently skip this exact version instead of postponing it.
        skip: bool,
    },
    /// Restore the terminal and exit. The core keeps running.
    Quit,
    /// Re-read everything a screen shows.
    Refresh(Screen),
    /// Force a fresh exit-IP lookup from Home.
    RefreshIp,
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
    /// Regenerate the active profile after startup, reloading a running core.
    SynchronizeConfig,
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
        /// Probe semantics.
        mode: ProbeMode,
    },
    /// Measure every member of a group.
    TestGroup {
        /// Group name.
        group: String,
        /// Probe semantics.
        mode: ProbeMode,
    },
    /// Measure every node the core knows about.
    TestAllNodes {
        /// Probe semantics.
        mode: ProbeMode,
    },
    /// Measure download speed through the currently active proxy route.
    TestRouteSpeed {
        /// Which backend and sample size to use.
        mode: SpeedMode,
    },
    /// Install speedtest-go after explicit confirmation.
    InstallSpeedtestGo,
    /// Run one entry of the tests screen.
    RunTest {
        /// Which check to run.
        kind: TestKind,
        /// What to run it against.
        target: String,
        /// Probe semantics for latency checks.
        mode: ProbeMode,
    },
    /// Abandon the running test batch.
    CancelTests,
    /// Clear cached unlock-check results.
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
    /// The path is built from `App::home`, mirroring `AppPaths::runtime_config`.
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
    /// Replace a remote source and download its configuration.
    EditProfileSource {
        /// Stable profile identifier.
        uid: String,
        /// New subscription URL.
        url: String,
    },
    /// Open a base's private override, or the current base if uid is absent.
    EditProfileOverride {
        /// Base profile uid; absent means the active profile.
        uid: Option<String>,
    },
    /// Persist a rule in the current base's override.
    AddProfileRule {
        /// Complete Mihomo rule text.
        rule: String,
    },
    /// Authenticate while the terminal is released, then retry the operation.
    AuthorizeCore {
        /// Core binary receiving the grant.
        binary: PathBuf,
        /// Comma-separated network capabilities.
        capabilities: String,
        /// Operation to retry after authorization.
        next: Box<Self>,
    },
    /// Persist an approved local DNS listener, then retry the interrupted operation.
    ResolveDnsConflict {
        /// Local replacement endpoint.
        address: String,
        /// Operation to retry.
        next: Box<Self>,
    },
}

impl Effect {
    /// A short verb, for tracing in the binary.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::CopyText { .. } | Self::CopyProfile { .. } | Self::CopyProxy { .. } => {
                "copy to clipboard"
            }
            Self::Startup => "startup",
            Self::CheckAppUpdate { .. } => "check application update",
            Self::InstallAppUpdate { .. } => "install application update",
            Self::DismissAppUpdate { .. } => "dismiss application update",
            Self::Quit => "quit",
            Self::Refresh(_) => "refresh",
            Self::RefreshIp => "refresh IP",
            Self::LoadProfiles => "load profiles",
            Self::SwitchProfile { .. } => "switch profile",
            Self::UpdateProfiles { .. } => "update profiles",
            Self::SetChain { .. } => "save chain",
            Self::PreviewConfig => "preview config",
            Self::ApplyConfig { .. } => "apply config",
            Self::PrepareConfig => "prepare config",
            Self::SynchronizeConfig => "synchronize config",
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
            Self::TestAllNodes { .. } => "test all nodes",
            Self::TestRouteSpeed { .. } => "route speed",
            Self::InstallSpeedtestGo => "install speedtest-go",
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
            Self::EditProfileSource { .. } => "edit subscription source",
            Self::EditProfileOverride { .. } => "edit profile override",
            Self::AddProfileRule { .. } => "add profile rule",
            Self::AuthorizeCore { .. } => "authorize core",
            Self::ResolveDnsConflict { .. } => "resolve DNS listener conflict",
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
    /// Offer a local listener without stopping the existing DNS owner.
    DnsConflict {
        /// Read-only classification and alternative.
        conflict: cvt_core::mihomo::listeners::DnsConflict,
        /// Operation to retry after confirmation.
        next: Box<Effect>,
    },
    /// A newer compatible application release was found.
    AppUpdateAvailable(UpdateRelease),
    /// Resume an approved operation after authentication or a core replacement.
    CoreAuthorized {
        /// Operation to retry.
        next: Box<Effect>,
    },
    /// An explicit capability grant must be approved before retrying.
    CoreAuthorization {
        /// Core binary receiving the grant.
        binary: PathBuf,
        /// Comma-separated network capabilities.
        capabilities: String,
        /// Operation to retry after authorization.
        next: Box<Effect>,
    },
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
    /// Public exit IP, obtained through Mihomo.
    IpInfo(IpInfo),
    /// The exit-IP lookup began.
    IpLookupStarted,
    /// Every configured exit-IP service failed.
    IpLookupFailed(String),
    /// Settings read from disk.
    Settings(Box<Settings>),
    /// A preview of the generated configuration.
    Preview(Box<Preview>),
    /// Directories that look like an existing `clash-verge-rev` home.
    ImportSources(Vec<PathBuf>),
    /// One test finished, or started.
    TestResult {
        /// Probe method in effect when this test started.
        mode: ProbeMode,
        /// Which check it was.
        kind: TestKind,
        /// What it ran against.
        target: String,
        /// How it ended.
        result: TestResult,
    },
    /// Current route throughput measurement finished.
    RouteSpeed(Result<String, String>),
    /// Selected speedtest-go mode requires an installed backend.
    SpeedtestMissing,
    /// A node probe completed; every visible occurrence of that node updates.
    NodeDelay {
        /// Probe semantics.
        mode: ProbeMode,
        /// Node name.
        name: String,
        /// Measured milliseconds, or no answer.
        delay: Option<u16>,
    },
    /// A note from the binary that is worth showing but is not a failure.
    Notice(String),
}

/// Exit IP information returned through the running proxy route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IpInfo {
    /// Public IP of the current route.
    pub ip: String,
    /// Country or region reported by the lookup service.
    pub country: String,
    /// Network operator, if provided.
    pub organization: String,
}

/// A route throughput backend and its download size, if applicable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeedMode {
    /// A small, low-data sample.
    Sample4,
    /// A medium sample.
    Sample20,
    /// A large sample.
    Sample100,
    /// The external speedtest-go backend.
    Speedtest,
}

impl SpeedMode {
    /// Labels are semantic keys translated by the TUI at display time.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Sample4 => "download sample · 4 MB",
            Self::Sample20 => "download sample · 20 MB",
            Self::Sample100 => "download sample · 100 MB",
            Self::Speedtest => "speedtest-go",
        }
    }

    /// Requested sample bytes. The external backend manages its own samples.
    pub const fn sample_bytes(self) -> Option<usize> {
        match self {
            Self::Sample4 => Some(4_000_000),
            Self::Sample20 => Some(20_000_000),
            Self::Sample100 => Some(100_000_000),
            Self::Speedtest => None,
        }
    }

    pub(super) const fn from_index(index: usize) -> Option<Self> {
        match index {
            0 => Some(Self::Sample4),
            1 => Some(Self::Sample20),
            2 => Some(Self::Sample100),
            3 => Some(Self::Speedtest),
            _ => None,
        }
    }

    pub(super) const fn index(self) -> usize {
        match self {
            Self::Sample4 => 0,
            Self::Sample20 => 1,
            Self::Sample100 => 2,
            Self::Speedtest => 3,
        }
    }
}

/// Something the binary finished.
///
/// Each variant becomes a status message, so the user learns that a request
/// that takes a second or more actually completed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Done {
    /// A desktop copy succeeded, or an OSC 52 request was sent.
    ClipboardCopied {
        /// Terminal permission cannot be confirmed.
        terminal: bool,
    },
    /// Application replacement succeeded; restart to use it.
    AppUpdated {
        /// Installed version.
        version: String,
        /// Preserved previous executable.
        backup: PathBuf,
    },
    /// Content contributing to the active configuration changed.
    ProfileContentChanged,
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
    ConnectionClosed {
        /// Controller connection identifier.
        id: String,
    },
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
    /// Optional route speed backend was installed and verified.
    SpeedtestInstalled {
        /// Release tag reported by GitHub.
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

/// A stable application update offered to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateRelease {
    /// Official release tag.
    pub tag: String,
    /// Brief release notes.
    pub summary: String,
    /// Official release page.
    pub url: String,
}
