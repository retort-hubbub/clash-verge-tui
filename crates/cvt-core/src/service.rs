//! The application facade.
//!
//! Everything above this line is a library: a config document, a merge engine,
//! a store, an API client, a process supervisor. [`Service`] is where those
//! become *operations a user asks for* — "apply this profile", "start the
//! core", "test these nodes" — and it is the only type the TUI and the CLI
//! need to hold.
//!
//! # Why the ordering logic lives here
//!
//! Applying a configuration has a decision tree that is easy to get subtly
//! wrong and expensive to get wrong in production:
//!
//! 1. generate and validate; refuse to touch anything if the result is broken;
//! 2. hand the document to the core over the API, which applies most changes
//!    without dropping connections;
//! 3. only if that fails, restart the process;
//! 4. if the core then fails to come up, restore the previous snapshot and
//!    start again — because "I edited my rules and now my network is gone" is
//!    the worst outcome this program can produce, and the snapshot is already
//!    on disk.
//!
//! [`Service::apply`] commits the generated document, then [`Service::reload`]
//! performs steps 2 to 4. Tests cover the decision path with a fake controller
//! and an optional real core.

mod backup;

pub use backup::{BACKUP_LIMIT, Backup};

use std::path::PathBuf;

use crate::enhance::pipeline::{Outcome, Pipeline};
use crate::error::{Error, Result};
use crate::mihomo::client::Client;
use crate::mihomo::endpoint::Endpoint;
use crate::mihomo::supervisor::{CoreStatus, Supervisor};
use crate::model::config::Config;
use crate::paths::AppPaths;
use crate::profile::store::ProfileStore;
use crate::settings::Settings;
/// How long one look at one group may take, how long the whole replay may
/// take, and how often to look again.
///
/// Three attempts got this wrong in three different ways, and the shape of the
/// answer is the third: a deadline per group multiplies (twenty groups that are
/// gone cost twenty waits); a single deadline for the whole replay hands
/// everything to the first group that is gone; and waiting on each group *in
/// turn* inside that deadline does the same thing more slowly — six groups the
/// subscription has removed cost six waits, and the seventh choice, the one
/// that would have worked, is never reached.
///
/// So nothing waits on a group. The ones that are there are replayed first, and
/// the rest are polled together until they answer or the total runs out, which
/// makes a group that is gone cost one read per round rather than a share of
/// the budget.
const REPLAY_PER_GROUP: std::time::Duration = std::time::Duration::from_millis(250);
const REPLAY_TOTAL: std::time::Duration = std::time::Duration::from_millis(2000);
const REPLAY_STEP: std::time::Duration = std::time::Duration::from_millis(25);

/// How long this group may take, given how much of the total is left.
fn group_budget(overall: std::time::Instant, limit: std::time::Duration) -> std::time::Instant {
    (std::time::Instant::now() + limit).min(overall)
}

/// Point a group at a member and make sure it took.
async fn replay_one(
    client: &Client,
    name: &str,
    member: &str,
    deadline: std::time::Instant,
) -> bool {
    if !select_within(client, name, member, deadline).await {
        return false;
    }
    confirm_selection(client, name, member, deadline).await
}

/// Choose a member, without letting the call outlive the budget.
///
/// The reads were given a deadline and this was not, which is the same mistake
/// one call further down: a core that answers `GET /group/…` and never answers
/// the `PUT` costs the *client's* timeout — ten seconds by default —
/// which is not the replay's budget and cannot be enforced from here.
async fn select_within(
    client: &Client,
    name: &str,
    member: &str,
    deadline: std::time::Instant,
) -> bool {
    let left = deadline.saturating_duration_since(std::time::Instant::now());
    if left.is_zero() {
        return false;
    }
    matches!(
        tokio::time::timeout(left, client.select(name, member)).await,
        Ok(Ok(()))
    )
}

/// Read a group, without letting one slow request outlive the budget.
///
/// The client has its own ten-second timeout;
/// a deadline this function cannot enforce is a deadline in name only.
async fn read_group(
    client: &Client,
    group: &str,
    deadline: std::time::Instant,
) -> Option<crate::mihomo::types::ProxyView> {
    let left = deadline.saturating_duration_since(std::time::Instant::now());
    if left.is_zero() {
        return None;
    }
    tokio::time::timeout(left, client.group(group))
        .await
        .ok()?
        .ok()
}

/// Wait until the core reports the member that was asked for.
///
/// A `select` group reports the choice as `now`. A `url-test` or `fallback`
/// group *pins* it instead and reports `fixed`, keeping `now` for whatever the
/// test last picked — so checking only `now` reported a pin that had taken as a
/// failure, which is how the first version of this managed to apply a choice
/// and count zero. Either field is the choice having taken.
async fn confirm_selection(
    client: &Client,
    group: &str,
    member: &str,
    deadline: std::time::Instant,
) -> bool {
    loop {
        match read_group(client, group, deadline).await {
            Some(view)
                if view.now.as_deref() == Some(member) || view.fixed.as_deref() == Some(member) =>
            {
                return true;
            }
            // Not there at all: this document does not have the group, and the
            // next look would fail the same way.
            None if std::time::Instant::now() >= deadline => return false,
            _ => {}
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(REPLAY_STEP).await;
    }
}

/// How a configuration change should reach the core.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReloadMode {
    /// Try the API first, restarting only if that fails. The default.
    Auto,
    /// Only ever use the API; a failure is reported rather than worked around.
    HotReload,
    /// Always restart the process.
    Restart,
}

impl ReloadMode {
    /// Short label for the status bar.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::HotReload => "hot reload",
            Self::Restart => "restart",
        }
    }
}

/// What actually happened when a configuration was applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReloadOutcome {
    /// The core accepted the document over the API.
    HotReloaded,
    /// The process was restarted.
    Restarted {
        /// The new process id.
        pid: u32,
    },
    /// The document failed to load, so the previous snapshot was restored and
    /// the core was started again with it.
    RolledBack {
        /// Why the new document was rejected.
        reason: String,
        /// The snapshot that was restored.
        snapshot: PathBuf,
    },
}

impl ReloadOutcome {
    /// `true` when the core is running the requested configuration.
    #[must_use]
    pub fn succeeded(&self) -> bool {
        matches!(self, Self::HotReloaded | Self::Restarted { .. })
    }

    /// One-line description for a status message.
    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            Self::HotReloaded => "reloaded without a restart".to_owned(),
            Self::Restarted { pid } => format!("restarted (pid {pid})"),
            Self::RolledBack { reason, .. } => {
                format!("rejected ({reason}); restored the previous configuration")
            }
        }
    }
}

/// The result of a full apply.
#[derive(Debug, Clone)]
pub struct ApplyReport {
    /// What the pipeline produced.
    pub outcome: Outcome,
    /// Whether the change reached the core, and how.
    pub reload: Option<ReloadOutcome>,
    /// `true` when the generated document was written to disk.
    pub written: bool,
    /// How many remembered node choices were replayed onto the core.
    ///
    /// Zero is the ordinary answer for a profile nobody has chosen a node in.
    /// It is reported rather than done silently because a *choice* is
    /// something a user made, and one that could not be replayed — a group the
    /// subscription renamed, a node it dropped — is worth being able to see.
    pub selections_restored: usize,
}

impl ApplyReport {
    /// `true` when the configuration is live.
    #[must_use]
    pub fn is_live(&self) -> bool {
        self.reload.as_ref().is_some_and(ReloadOutcome::succeeded)
    }
}

/// The headless application.
#[derive(Debug, Clone)]
pub struct Service {
    paths: AppPaths,
    settings: Settings,
}

impl Service {
    /// Open the application home, creating directories and loading settings.
    ///
    /// # Errors
    /// [`Error::Io`] when the home cannot be created, [`Error::Parse`] when
    /// the settings file is malformed.
    pub fn open(paths: AppPaths) -> Result<Self> {
        paths.ensure_dirs()?;
        let settings = Settings::load(&paths)?;
        Ok(Self { paths, settings })
    }

    /// Open an already-resolved service, skipping validation.
    ///
    /// # Errors
    /// As [`Service::open`].
    pub fn open_default() -> Result<Self> {
        Self::open(AppPaths::resolve(None)?)
    }

    /// The application home.
    #[must_use]
    pub fn paths(&self) -> &AppPaths {
        &self.paths
    }

    /// Current settings.
    #[must_use]
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Replace the settings in memory. Call [`Service::save_settings`] to
    /// persist them.
    pub fn set_settings(&mut self, settings: Settings) {
        self.settings = settings;
    }

    /// Persist the settings, validating first.
    ///
    /// # Errors
    /// [`Error::InvalidValue`] when they are invalid, otherwise propagates the
    /// write failure.
    pub fn save_settings(&self) -> Result<()> {
        self.settings.save(&self.paths)
    }

    /// Load the profile store.
    ///
    /// # Errors
    /// [`Error::Parse`] when the index is malformed.
    pub fn store(&self) -> Result<ProfileStore> {
        ProfileStore::load(&self.paths)
    }

    /// The process supervisor.
    #[must_use]
    pub fn supervisor(&self) -> Supervisor {
        Supervisor::new(self.paths.clone())
    }

    /// The pipeline that generates runtime configurations.
    #[must_use]
    pub fn pipeline(&self) -> Pipeline {
        Pipeline::new(self.paths.clone()).with_control_plane(
            self.settings.core.external_controller.as_deref(),
            self.settings.core.secret.as_deref(),
        )
    }

    /// Where the controller is, as far as can be determined.
    ///
    /// Reads, in order: the generated runtime configuration, then the profile
    /// that is currently selected. A generated file is authoritative because
    /// it is what the running core was actually launched with.
    ///
    /// # Errors
    /// [`Error::Io`] or [`Error::Parse`] when the runtime configuration exists
    /// but cannot be read.
    pub fn endpoint(&self) -> Result<Option<Endpoint>> {
        let runtime = self.paths.runtime_config();
        if runtime.is_file() {
            let text = self.paths.read(&runtime)?;
            let config = Config::from_yaml(&text)?;
            if let Some(endpoint) = Endpoint::from_config(&config) {
                return Ok(Some(endpoint));
            }
        }
        let store = self.store()?;
        let Some(current) = store.current() else {
            return Ok(None);
        };
        let Ok(text) = store.read_document(current) else {
            return Ok(None);
        };
        Ok(Config::from_yaml(&text)
            .ok()
            .and_then(|c| Endpoint::from_config(&c)))
    }

    /// A client for the controller.
    ///
    /// # Errors
    /// [`Error::MissingField`] when no configuration declares a controller,
    /// so the caller can tell "not configured yet" from "unreachable".
    pub fn client(&self) -> Result<Client> {
        let endpoint = self.endpoint()?.ok_or_else(|| Error::MissingField {
            uid: "-".to_owned(),
            field: "external-controller",
        })?;
        Client::new(endpoint)
    }

    /// The deployed core's local HTTP-capable proxy listener, for subscription
    /// fallback. The controller port is a different service; a SOCKS-only port
    /// cannot be passed to the HTTP proxy client.
    #[must_use]
    pub fn proxy_addr(&self) -> Option<String> {
        let text = self.paths.read(&self.paths.runtime_config()).ok()?;
        let config = Config::from_yaml(&text).ok()?;
        config
            .mixed_port()
            .or_else(|| config.port())
            .map(|port| format!("127.0.0.1:{port}"))
    }

    /// Generate a runtime configuration without writing anything.
    ///
    /// # Errors
    /// Propagates pipeline failures.
    pub fn generate(&self) -> Result<Outcome> {
        let store = self.store()?;
        self.pipeline().generate(&store)
    }

    /// Report the core's running state.
    #[must_use]
    pub fn core_status(&self) -> CoreStatus {
        self.supervisor().status()
    }

    /// Locate the core binary, honouring the configured override.
    #[must_use]
    pub fn core_binary(&self) -> Option<PathBuf> {
        self.supervisor().locate_with(
            self.settings.core.binary.as_deref(),
            self.settings.core.use_managed,
        )
    }

    /// Start the core using the generated configuration.
    ///
    /// The configuration is validated with `mihomo -t` first: a crash loop is
    /// far harder to diagnose than one error message.
    ///
    /// # Errors
    /// [`Error::CoreUnavailable`] when nothing to run can be found,
    /// [`Error::Validation`] when the document has errors, and
    /// [`Error::ProcessFailed`] when validation or the launch fails.
    pub fn start_core(&self) -> Result<u32> {
        let supervisor = self.supervisor();
        let binary = self.core_binary().ok_or_else(|| Error::CoreUnavailable {
            reason: format!(
                "no mihomo binary found; put one at {} or set {}",
                self.paths.core_dir().join("mihomo").display(),
                crate::mihomo::supervisor::CORE_ENV
            ),
        })?;
        let config = self.paths.runtime_config();
        if !config.is_file() {
            // Selecting a subscription makes it current but does not write a
            // runtime document. Starting from a fresh home should complete
            // that first apply, using the same validation as an explicit apply.
            let outcome = self.generate()?;
            self.pipeline().commit(&outcome, false)?;
        }
        let text = self.paths.read(&config)?;
        let parsed = Config::from_yaml(&text)?;
        let report = crate::validate::check(&parsed);
        if !report.is_ok() {
            return Err(Error::Validation {
                problems: report.errors_iter().map(|d| d.message.clone()).collect(),
            });
        }
        // Before the rotation, not after: `Supervisor::start` is where the
        // "already running" check lives, and rotating first meant a *refused*
        // start moved the log of the core that is still running — which then
        // kept writing into `.1`, with nothing left to create `core.log`,
        // because the start that would have opened it never happened.
        if let CoreStatus::Running { pid, .. } = supervisor.status() {
            return Err(Error::Unsupported(format!(
                "the core is already running as pid {pid}; stop it first"
            )));
        }
        supervisor.validate_config(&binary, &config)?;
        // Logs are rotated here or never: the child holds its log open for as
        // long as it runs, so this is the only moment either file can be moved
        // without a live process writing into a file nobody will read.
        self.rotate_logs(&supervisor);
        supervisor.start(&binary, &config)
    }

    /// Rotate and prune both logs, best effort.
    ///
    /// Deliberately not fallible: a user who cannot rotate a log file still
    /// wants their core started, and a full disk that stops a log from moving
    /// is not a reason to refuse to run. What did happen is logged.
    fn rotate_logs(&self, supervisor: &Supervisor) {
        let settings = &self.settings.logs;
        for log in [self.paths.core_log(), self.paths.app_log()] {
            match supervisor.rotate_log(&log, settings.max_size_bytes, settings.keep) {
                Ok(Some(path)) => tracing::info!(file = %path.display(), "log rotated"),
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(log = %log.display(), error = %error, "could not rotate the log");
                }
            }
            match supervisor.prune_logs(&log, settings.keep_days) {
                Ok(0) => {}
                Ok(removed) => tracing::info!(removed, "old rotated logs deleted"),
                Err(error) => {
                    tracing::warn!(log = %log.display(), error = %error, "could not prune the logs");
                }
            }
        }
    }

    /// Stop the core.
    ///
    /// # Errors
    /// Propagates a signal failure.
    pub fn stop_core(&self) -> Result<bool> {
        self.supervisor().stop()
    }

    /// Stop and start the core.
    ///
    /// # Errors
    /// Propagates stop and start failures.
    pub fn restart_core(&self) -> Result<u32> {
        self.stop_core()?;
        self.start_core()
    }

    /// Generate, write, and hand the configuration to the core.
    ///
    /// # Errors
    /// [`Error::Validation`] when the result has errors and `force` is not
    /// set; otherwise propagates generation or commit failures.
    pub async fn apply(&self, force: bool, mode: ReloadMode) -> Result<ApplyReport> {
        let outcome = self.generate()?;
        let pipeline = self.pipeline();
        pipeline.commit(&outcome, force)?;
        let reload = self.reload(mode).await?;
        // A reload rebuilds every group, so the choice a user made this morning
        // is gone by the afternoon. It is replayed here rather than at each
        // caller, because "apply" is the operation that discards it.
        //
        // Not after a rollback, though: the core is running the document that
        // was there *before* this apply, and the choices just read belong to
        // the profile that failed to apply. Replaying them would point a
        // restored configuration at members it may not have.
        let selections_restored = if let ReloadOutcome::RolledBack { .. } = &reload {
            0
        } else {
            {
                // Before the choices, and before this returns. `/version`
                // answering means the *process* is up, not that it has this
                // configuration: a reload rebuilds the groups in the
                // background, and for a moment afterwards a group the document
                // declares is not there. Reporting success in that window made
                // `apply` mean "the file was written", while the very next
                // command failed with `no group named PROXY` — so the wait is
                // for the document, not for the process.
                self.wait_for_document(&outcome.config).await;
                self.restore_selections().await.unwrap_or(0)
            }
        };
        Ok(ApplyReport {
            outcome,
            reload: Some(reload),
            written: true,
            selections_restored,
        })
    }

    /// Record a node choice on the current profile.
    ///
    /// # Errors
    /// Whatever reading or writing the profile index returns.
    pub fn remember_selection(&self, group: &str, member: &str) -> Result<()> {
        let mut store = self.store()?;
        store.remember_selection(group, member)?;
        store.save()
    }

    /// Forget a group's choice, which is what unpinning means.
    ///
    /// # Errors
    /// Whatever reading or writing the profile index returns.
    pub fn forget_selection(&self, group: &str) -> Result<()> {
        let mut store = self.store()?;
        store.forget_selection(group)?;
        store.save()
    }

    /// Replay the current profile's remembered choices onto the core.
    ///
    /// Best effort per group: a group the subscription has renamed, or a member
    /// it has dropped, is skipped rather than failing the others — the choice
    /// was made against a document that no longer exists, and the remaining
    /// ones are still good.
    ///
    /// # Errors
    /// [`Error::ControllerUnreachable`] when there is no core to talk to.
    pub async fn restore_selections(&self) -> Result<usize> {
        let selections = self.store()?.selections();
        if selections.is_empty() {
            return Ok(0);
        }
        let client = self.client()?;
        let overall = std::time::Instant::now() + REPLAY_TOTAL;
        let mut applied = 0;
        let mut late = Vec::new();

        // Two passes, and both are load-bearing.
        //
        // A group that is not there yet is not necessarily gone: a reload
        // rebuilds every group and the core applies it in the background, so
        // the whole reason for waiting is that the window is real. But a group
        // the subscription has *removed* is also not there, and it never will
        // be — and one pass with a budget per group let it spend that budget
        // and starve every choice after it. So the ones that are present are
        // replayed first, and only what is left is waited for.
        for selection in selections {
            if std::time::Instant::now() >= overall {
                tracing::debug!("the replay budget is spent");
                break;
            }
            let deadline = group_budget(overall, REPLAY_PER_GROUP);
            match read_group(&client, &selection.name, deadline).await {
                Some(_) => {
                    if replay_one(&client, &selection.name, &selection.now, deadline).await {
                        applied += 1;
                    }
                }
                None => late.push(selection),
            }
        }

        // Everything that was not there, polled *together* rather than one
        // after another. Waiting on each in turn gives the first group that is
        // gone the whole of its own budget and then the next one the same:
        // six groups a subscription has removed cost six waits, the total runs
        // out, and the seventh choice — the one that would have worked — is
        // never reached. A round costs one read per group still pending, and a
        // group that answers 404 costs almost nothing, so the live one is
        // replayed on the round after it appears however many dead ones precede
        // it.
        let mut pending = late;
        while !pending.is_empty() && std::time::Instant::now() < overall {
            let mut still = Vec::new();
            for selection in pending {
                let deadline = group_budget(overall, REPLAY_PER_GROUP);
                if read_group(&client, &selection.name, deadline)
                    .await
                    .is_none()
                {
                    still.push(selection);
                    continue;
                }
                if replay_one(&client, &selection.name, &selection.now, deadline).await {
                    applied += 1;
                } else {
                    tracing::debug!(
                        group = %selection.name,
                        member = %selection.now,
                        "the choice did not take; the configuration may have replaced the group"
                    );
                }
            }
            if still.is_empty() {
                break;
            }
            if std::time::Instant::now() >= overall {
                tracing::debug!(
                    groups = still.len(),
                    "the replay budget is spent before these groups came back"
                );
                break;
            }
            pending = still;
            tokio::time::sleep(REPLAY_STEP).await;
        }

        Ok(applied)
    }
    /// Hand the already-written runtime configuration to the core.
    ///
    /// Implements the decision tree described in the module docs. Never
    /// touches the filesystem beyond reading the snapshots it restores.
    ///
    /// # Errors
    /// [`Error::ControllerUnreachable`] when the core is not running and
    /// cannot be started, or when a restart does not come up in time;
    /// [`Error::CoreUnavailable`] when no core binary can be found. A failed
    /// reload whose cause cannot be undone reports *that* cause, never a
    /// failure of the rollback bookkeeping.
    pub async fn reload(&self, mode: ReloadMode) -> Result<ReloadOutcome> {
        // Reload operates on an already deployed document. `start_core` may
        // prepare a first document for the user, but doing that here would
        // silently turn a reload of nothing into a new apply.
        if !self.paths.runtime_config().is_file() {
            return Err(Error::invalid(
                "runtime config",
                "no configuration has been generated yet; apply a profile first",
            ));
        }
        match mode {
            ReloadMode::Restart => return self.restart_with_rollback().await,
            ReloadMode::HotReload => {
                self.hot_reload().await?;
                return Ok(ReloadOutcome::HotReloaded);
            }
            ReloadMode::Auto => {}
        }

        match self.hot_reload().await {
            Ok(()) => Ok(ReloadOutcome::HotReloaded),
            Err(reason) => {
                let reason = reason.to_string();
                match self.restart_with_rollback().await {
                    Ok(ReloadOutcome::Restarted { pid }) => Ok(ReloadOutcome::Restarted { pid }),
                    Ok(other) => Ok(other),
                    Err(e) => {
                        if !self.settings.core.rollback_on_failure || !e.is_rollbackable() {
                            return Err(e);
                        }
                        // The core would not come up with the new document, so
                        // put back what was working. A first apply has nothing
                        // to put back: report the restart failure — the only
                        // real information there is — rather than replacing it
                        // with a complaint about a missing snapshot.
                        if self.pipeline().snapshots()?.is_empty() {
                            return Err(e);
                        }
                        let snapshot = self.pipeline().rollback()?;
                        self.stop_core()?;
                        self.start_core()?;
                        Ok(ReloadOutcome::RolledBack { reason, snapshot })
                    }
                }
            }
        }
    }

    /// Ask the core to re-read the generated document, without restarting.
    ///
    /// # Errors
    /// [`Error::ControllerUnreachable`] when the core is not running.
    pub async fn hot_reload(&self) -> Result<()> {
        if !self.core_status().is_running() {
            return Err(Error::ControllerUnreachable {
                endpoint: self
                    .endpoint()?
                    .map_or_else(|| "unknown".to_owned(), |e| e.describe()),
                source: "the core process is not running".into(),
            });
        }
        let client = self.client()?;
        if self.settings.update.close_connections_on_apply {
            // Best effort: a core that refuses this is still reloadable.
            let _ = client.close_all_connections().await;
        }
        client
            .reload_configs(Some(&self.paths.runtime_config()), None, true)
            .await
    }

    /// Restart the core, waiting for the API to come back.
    async fn restart_with_rollback(&self) -> Result<ReloadOutcome> {
        let pid = self.restart_core()?;
        self.wait_until_ready().await?;
        Ok(ReloadOutcome::Restarted { pid })
    }

    /// Wait until the core answers for the groups this document declares.
    ///
    /// Best effort and bounded, in the shape the replay needed three attempts
    /// to find: **no call is outside the deadline**, and **no group waits on
    /// its own**. Both halves matter and both were missing here first.
    ///
    /// A sequential wait gives the first declared group that never appears the
    /// whole budget, so the groups after it are never asked about — the same
    /// starvation the replay had. And a deadline checked *between* calls is a
    /// deadline this function cannot enforce: `client.group` carries the
    /// client's own timeout, which is ten seconds by default,
    /// so a core that answers `/version` and hangs `/group` held an `apply`
    /// for twice the budget it was supposed to have.
    async fn wait_for_document(&self, config: &Config) {
        let Ok(client) = self.client() else {
            return;
        };
        let groups = config.proxy_groups();
        let mut pending: Vec<&str> = groups.iter().map(|group| group.name.as_str()).collect();
        if pending.is_empty() {
            return;
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !pending.is_empty() && std::time::Instant::now() < deadline {
            let mut still = Vec::new();
            for name in pending {
                let left = deadline.saturating_duration_since(std::time::Instant::now());
                if left.is_zero() {
                    still.push(name);
                    continue;
                }
                match tokio::time::timeout(left, client.group(name)).await {
                    Ok(Ok(_)) => {}
                    Ok(Err(_)) => still.push(name),
                    // The deadline itself: nothing more will be waited for.
                    Err(_) => {
                        tracing::debug!(group = name, "the core never served this group");
                        return;
                    }
                }
            }
            if still.is_empty() {
                return;
            }
            if std::time::Instant::now() >= deadline {
                tracing::debug!(
                    groups = still.len(),
                    "the configuration is live except for these groups"
                );
                return;
            }
            pending = still;
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    }

    /// Wait for the controller to answer after a restart.
    ///
    /// The core binds its listener a moment after the process starts, so a
    /// request immediately after `start` legitimately fails.
    ///
    /// # Errors
    /// [`Error::ControllerUnreachable`] when it never answers within the
    /// deadline.
    pub async fn wait_until_ready(&self) -> Result<()> {
        self.client()?.wait_until_ready().await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::profile::item::PrfItem;
    use chrono::Utc;
    use tempfile::TempDir;

    const BASE: &str = r#"
mixed-port: 7890
external-controller: 127.0.0.1:9090
mode: rule
proxies:
  - { name: "JP 01", type: vless, server: 1.2.3.4, port: 443, uuid: u }
proxy-groups:
  - { name: PROXY, type: select, proxies: ["JP 01", DIRECT] }
rules:
  - MATCH,PROXY
"#;

    struct Fixture {
        _dir: TempDir,
        service: Service,
    }

    fn fixture() -> Fixture {
        let dir = TempDir::new().unwrap();
        let service = Service::open(AppPaths::new(dir.path())).unwrap();
        Fixture { _dir: dir, service }
    }

    impl Fixture {
        fn seed(&self) -> String {
            let mut store = self.service.store().unwrap();
            let uid = store.add(PrfItem::local("L1", "base"));
            let item = store.get(&uid).unwrap().clone();
            store.write_document(&item, BASE).unwrap();
            store.set_current(&uid).unwrap();
            store.save().unwrap();
            uid
        }
    }

    #[test]
    fn opening_creates_the_home_and_loads_default_settings() {
        let f = fixture();
        assert!(f.service.paths().profiles_dir().is_dir());
        assert_eq!(*f.service.settings(), Settings::default());
        assert!(f.service.store().unwrap().items().is_empty());
    }

    #[test]
    fn a_malformed_settings_file_stops_the_service_from_opening() {
        let dir = TempDir::new().unwrap();
        let paths = AppPaths::new(dir.path());
        paths.ensure_dirs().unwrap();
        std::fs::write(paths.settings_file(), "core: [oops\n").unwrap();
        let err = Service::open(paths).unwrap_err();
        assert!(matches!(err, Error::Parse { .. }), "{err:?}");
    }

    #[test]
    fn settings_survive_a_round_trip_through_the_service() {
        let mut f = fixture();
        let mut s = f.service.settings().clone();
        s.ui.refresh_ms = 250;
        f.service.set_settings(s.clone());
        f.service.save_settings().unwrap();
        let reopened = Service::open(f.service.paths().clone()).unwrap();
        assert_eq!(*reopened.settings(), s);
    }

    #[test]
    fn an_invalid_setting_is_refused_before_it_reaches_the_disk() {
        let mut f = fixture();
        let mut s = f.service.settings().clone();
        s.test.concurrency = 0;
        f.service.set_settings(s);
        assert!(f.service.save_settings().is_err());
        assert!(!f.service.paths().settings_file().exists());
    }

    #[test]
    fn there_is_no_endpoint_before_anything_is_generated() {
        let f = fixture();
        assert!(f.service.endpoint().unwrap().is_none());
        let err = f.service.client().unwrap_err();
        assert!(
            matches!(
                err,
                Error::MissingField {
                    field: "external-controller",
                    ..
                }
            ),
            "{err:?}"
        );
    }

    #[test]
    fn the_generated_runtime_config_is_the_authority_on_the_endpoint() {
        let f = fixture();
        f.seed();
        let outcome = f.service.generate().unwrap();
        f.service.pipeline().commit(&outcome, false).unwrap();
        let endpoint = f.service.endpoint().unwrap().unwrap();
        assert_eq!(endpoint, Endpoint::tcp("127.0.0.1:9090", None));
        assert!(endpoint.is_loopback());
    }

    #[test]
    fn subscription_fallback_uses_the_proxy_listener_not_the_controller() {
        let f = fixture();
        f.seed();
        assert_eq!(f.service.proxy_addr(), None, "only deployed ports count");
        let outcome = f.service.generate().unwrap();
        f.service.pipeline().commit(&outcome, false).unwrap();
        assert_eq!(f.service.proxy_addr().as_deref(), Some("127.0.0.1:7890"));
        assert_eq!(
            f.service.endpoint().unwrap().unwrap(),
            Endpoint::tcp("127.0.0.1:9090", None)
        );

        std::fs::write(
            f.service.paths().runtime_config(),
            "socks-port: 7891\nexternal-controller: 127.0.0.1:9090\n",
        )
        .unwrap();
        assert_eq!(f.service.proxy_addr(), None, "SOCKS is not an HTTP proxy");
    }

    #[test]
    fn the_endpoint_falls_back_to_the_selected_profile() {
        let f = fixture();
        f.seed();
        // Nothing generated yet, but the profile declares a controller.
        let endpoint = f.service.endpoint().unwrap().unwrap();
        assert_eq!(endpoint, Endpoint::tcp("127.0.0.1:9090", None));
    }

    #[test]
    fn a_client_can_be_built_once_an_endpoint_is_known() {
        let f = fixture();
        f.seed();
        let client = f.service.client().unwrap();
        assert!(client.endpoint().is_loopback());
    }

    #[test]
    fn core_status_is_reported_without_a_binary() {
        let f = fixture();
        assert_eq!(f.service.core_status(), CoreStatus::Stopped);
        assert!(
            f.service.core_binary().is_none(),
            "nothing is on PATH in the test home"
        );
    }

    #[test]
    fn starting_without_a_binary_explains_how_to_fix_it() {
        let f = fixture();
        f.seed();
        f.service
            .pipeline()
            .commit(&f.service.generate().unwrap(), false)
            .unwrap();
        let err = f.service.start_core().unwrap_err();
        match err {
            Error::CoreUnavailable { reason } => {
                assert!(reason.contains("mihomo"), "{reason}");
                assert!(
                    reason.contains("CVT_CORE"),
                    "the message names the override: {reason}"
                );
            }
            other => panic!("expected CoreUnavailable, got {other:?}"),
        }
    }

    #[test]
    fn starting_without_a_generated_configuration_is_refused() {
        let f = fixture();
        let err = f.service.start_core().unwrap_err();
        // The missing binary is detected first, which is the more useful
        // message when both are missing.
        assert!(matches!(err, Error::CoreUnavailable { .. }), "{err:?}");
    }

    #[cfg(unix)]
    #[test]
    fn starting_a_selected_profile_generates_the_first_runtime_configuration() {
        use std::os::unix::fs::PermissionsExt as _;

        let f = fixture();
        f.seed();
        let binary = f.service.paths().core_dir().join("mihomo");
        std::fs::write(
            &binary,
            "#!/bin/sh\nif [ \"$1\" = \"-t\" ]; then exit 0; fi\nsleep 30\n",
        )
        .unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!f.service.paths().runtime_config().exists());

        let started = f.service.start_core();
        assert!(started.is_ok(), "{started:?}");
        assert!(f.service.paths().runtime_config().is_file());
        assert!(
            f.service
                .paths()
                .read(&f.service.paths().runtime_config())
                .unwrap()
                .contains("MATCH,PROXY")
        );
        f.service.stop_core().unwrap();
    }

    #[test]
    fn an_invalid_generated_configuration_is_refused_before_the_core_sees_it() {
        let f = fixture();
        let mut store = f.service.store().unwrap();
        let uid = store.add(PrfItem::local("L1", "broken"));
        let item = store.get(&uid).unwrap().clone();
        store
            .write_document(
                &item,
                "mixed-port: 7890\nexternal-controller: 127.0.0.1:9090\nrules:\n  - DOMAIN,a.test,GHOST\n",
            )
            .unwrap();
        store.set_current(&uid).unwrap();
        store.save().unwrap();

        // Force the document onto disk despite the validation errors.
        f.service
            .pipeline()
            .commit(&f.service.generate().unwrap(), true)
            .unwrap();

        // A fake core binary, so the binary check passes and validation runs.
        let fake = f.service.paths().core_dir().join("mihomo");
        std::fs::write(&fake, "#!/bin/sh\nexit 0\n").unwrap();
        let err = f.service.start_core().unwrap_err();
        assert!(matches!(err, Error::Validation { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn a_hot_reload_against_a_dead_core_fails_rather_than_pretending() {
        let f = fixture();
        f.seed();
        f.service
            .pipeline()
            .commit(&f.service.generate().unwrap(), false)
            .unwrap();
        let err = f.service.hot_reload().await.unwrap_err();
        assert!(
            matches!(err, Error::ControllerUnreachable { .. }),
            "{err:?}"
        );
        assert!(err.to_string().contains("not running"), "{err}");
    }

    #[tokio::test]
    async fn waiting_for_a_core_that_never_answers_times_out_with_a_reason() {
        let f = fixture();
        f.seed();
        f.service
            .pipeline()
            .commit(&f.service.generate().unwrap(), false)
            .unwrap();
        // Nothing listens on 127.0.0.1:9090 in the test environment.
        let err = f.service.wait_until_ready().await.unwrap_err();
        assert!(
            matches!(err, Error::ControllerUnreachable { .. }),
            "{err:?}"
        );
        assert!(err.to_string().contains("did not become ready"), "{err}");
    }

    #[test]
    fn apply_without_a_current_profile_reports_the_missing_base() {
        let f = fixture();
        let err = f.service.generate().unwrap_err();
        assert!(err.to_string().contains("current"), "{err}");
    }

    #[tokio::test]
    async fn replaying_nothing_asks_the_core_for_nothing() {
        // The ordinary answer for a profile nobody has chosen a node in: no
        // core is needed, and none is dialled — the check is that this returns
        // without one rather than that it returns a particular number.
        let f = fixture();
        f.seed();
        assert_eq!(f.service.restore_selections().await.unwrap(), 0);
    }

    #[test]
    fn the_backup_just_taken_is_never_pruned_by_its_own_prune() {
        let f = fixture();
        f.seed();
        // Five entries named *later* than now: a clock that ran fast, an archive
        // restored from another machine, a hand-made directory. The new backup
        // sorts last, so pruning by age alone deletes the one thing the call
        // exists to produce and returns a path to nothing.
        let future = Utc::now().timestamp() + 1000;
        for offset in 0..BACKUP_LIMIT {
            let dir = f
                .service
                .paths()
                .backups_dir()
                .join((future + i64::try_from(offset).unwrap_or(i64::MAX)).to_string());
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("profiles.yaml"), "current: L1\nitems: []\n").unwrap();
        }

        let created = f.service.backup().unwrap();
        assert!(
            created.join("profiles.yaml").is_file(),
            "`backup()` returned {} and pruned it away",
            created.display()
        );
        assert!(
            f.service
                .backups()
                .unwrap()
                .iter()
                .any(|b| b.path == created),
            "and it is not even listed"
        );
    }

    #[test]
    fn a_restore_keeps_the_state_it_replaces_even_when_the_backups_directory_is_full() {
        let f = fixture();
        f.seed();
        f.service.save_settings().unwrap();
        let future = Utc::now().timestamp() + 1000;
        // A real backup to restore from, and five entries named *later* than it
        // — so the safety copy `restore` takes, stamped now, sorts last of all.
        let source = f.service.paths().backups_dir().join(future.to_string());
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("profiles.yaml"), "current: L1\nitems: []\n").unwrap();
        std::fs::write(source.join("cvt.yaml"), "ui:\n  refresh_ms: 250\n").unwrap();
        for offset in 1..=BACKUP_LIMIT {
            let dir = f
                .service
                .paths()
                .backups_dir()
                .join((future + i64::try_from(offset).unwrap_or(i64::MAX)).to_string());
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("profiles.yaml"), "current: L1\nitems: []\n").unwrap();
        }

        let safety = f.service.restore(&source).unwrap();
        assert!(
            safety.join("profiles.yaml").is_file() && safety.join("cvt.yaml").is_file(),
            "the restore said the state it replaced was kept at {}, and it is not there",
            safety.display()
        );
    }

    #[test]
    fn restoring_the_oldest_backup_reads_it_before_pruning() {
        let f = fixture();
        f.service.save_settings().unwrap();
        let before = std::fs::read(f.service.paths().settings_file()).unwrap();
        let saved = "ui:\n  refresh_ms: 250\n";
        for stamp in 1..=BACKUP_LIMIT {
            let path = f.service.paths().backups_dir().join(stamp.to_string());
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join("cvt.yaml"), saved).unwrap();
        }

        let source = f.service.paths().backups_dir().join("1");
        let safety = f.service.restore(&source).unwrap();
        assert_eq!(
            std::fs::read_to_string(f.service.paths().settings_file()).unwrap(),
            saved,
            "creating the safety backup must not prune the restore source before it is read"
        );
        assert_eq!(std::fs::read(safety.join("cvt.yaml")).unwrap(), before);
        assert_eq!(f.service.backups().unwrap().len(), BACKUP_LIMIT);
    }

    /// A core that answers nothing must not hold an apply for twice its budget.
    ///
    /// `wait_for_document` checked its deadline *between* `client.group()`
    /// calls, and that call carries the client's own ten-second timeout. So a
    /// core that answered `/version` and hung `/group` held `apply` for about
    /// ten: a deadline the function could not
    /// enforce, which is the same mistake the selection replay was fixed for
    /// three times over.
    #[tokio::test]
    async fn waiting_for_a_document_is_bounded_by_its_own_deadline() {
        // A listener that accepts and never answers: the shape a reloading core
        // has while it is rebuilding its groups.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            // Held open and never answered, which is the shape being tested.
            let mut held = Vec::new();
            while let Ok((stream, _)) = listener.accept().await {
                held.push(stream);
            }
            drop(held);
        });

        let mut f = fixture();
        f.seed();
        let mut settings = f.service.settings().clone();
        settings.ui.refresh_ms = 1000;
        settings.core.external_controller = Some(format!("127.0.0.1:{port}"));
        f.service.set_settings(settings);
        let config = Config::from_yaml(
            "proxy-groups:\n  - {name: a, type: select, proxies: [DIRECT]}\n  \
             - {name: b, type: select, proxies: [DIRECT]}\nrules: ['MATCH,a']\n",
        )
        .unwrap();

        let started = std::time::Instant::now();
        f.service.wait_for_document(&config).await;
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(7),
            "the wait took {elapsed:?} for a five-second budget; a call is \
             outside the deadline"
        );
    }

    #[test]
    fn a_backup_holds_what_a_person_would_have_to_recreate() {
        let f = fixture();
        f.seed();
        // A home that has never saved its settings has no settings file to
        // copy, which is correct — the defaults are implied — so the realistic
        // case is the one worth asserting on.
        f.service.save_settings().unwrap();
        let outcome = f.service.generate().unwrap();
        f.service.pipeline().commit(&outcome, false).unwrap();

        let path = f.service.backup().unwrap();
        for name in ["cvt.yaml", "profiles.yaml", "profiles", "overrides"] {
            assert!(
                path.join(name).exists(),
                "{name} is missing from the backup"
            );
        }
        // Derived, large and reproducible: not the point of a backup, and the
        // reason the two directories are documented as different things.
        assert!(!path.join("runtime").exists());
        assert!(!path.join("logs").exists());
        assert_eq!(f.service.backups().unwrap().len(), 1);
    }

    #[test]
    fn restoring_puts_the_state_back_and_keeps_what_it_replaced() {
        let f = fixture();
        let uid = f.seed();
        let taken = f.service.backup().unwrap();
        std::fs::write(
            f.service.paths().profiles_dir().join("tail.yaml"),
            "later\n",
        )
        .unwrap();

        let safety = f.service.restore(&taken).unwrap();
        // The state being replaced is kept, so restoring the wrong backup is
        // itself undoable: `tail.yaml` is in the safety copy…
        assert!(safety.join("profiles").join("tail.yaml").exists());
        // …and it is still where it was, because a restore is additive. It is
        // now a document no index entry mentions, which this program preserves
        // on purpose — see the method's documentation.
        assert!(f.service.paths().profiles_dir().join("tail.yaml").exists());
        assert!(f.service.store().unwrap().get(&uid).is_some());
    }

    #[test]
    fn a_directory_that_is_not_a_backup_is_refused() {
        let f = fixture();
        f.seed();
        let elsewhere = tempfile::TempDir::new().unwrap();
        let error = f.service.restore(elsewhere.path()).unwrap_err();
        assert!(
            error.to_string().contains("not a backup"),
            "a restore has to say why it refused: {error}"
        );
    }

    #[test]
    fn only_the_newest_backups_are_kept() {
        let f = fixture();
        f.seed();
        // Five is the limit, and the directories are named by the second they
        // were taken, so this has to stand on one per second to make five
        // distinct ones.
        for _ in 0..BACKUP_LIMIT + 2 {
            // Never fails: a second backup in the same second is given a
            // suffixed name rather than refused.
            f.service.backup().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(1100));
        }
        let kept = f.service.backups().unwrap();
        assert_eq!(kept.len(), BACKUP_LIMIT, "the oldest two should be gone");
        assert!(
            !kept.iter().any(|backup| backup.name().is_empty()),
            "a backup with no name could not be restored by name"
        );
    }

    #[test]
    fn reload_mode_labels_are_stable() {
        assert_eq!(ReloadMode::Auto.label(), "auto");
        assert_eq!(ReloadMode::HotReload.label(), "hot reload");
        assert_eq!(ReloadMode::Restart.label(), "restart");
    }

    #[test]
    fn reload_outcomes_describe_themselves() {
        assert!(ReloadOutcome::HotReloaded.succeeded());
        assert!(ReloadOutcome::Restarted { pid: 7 }.succeeded());
        assert_eq!(
            ReloadOutcome::Restarted { pid: 7 }.summary(),
            "restarted (pid 7)"
        );

        let rolled = ReloadOutcome::RolledBack {
            reason: "bad yaml".to_owned(),
            snapshot: PathBuf::from("/tmp/s.yaml"),
        };
        assert!(!rolled.succeeded(), "a rollback means the request failed");
        assert!(
            rolled.summary().contains("restored"),
            "{}",
            rolled.summary()
        );
    }

    #[test]
    fn the_supervisor_and_pipeline_share_the_service_home() {
        let f = fixture();
        assert_eq!(
            f.service.supervisor().log_file(),
            f.service.paths().core_log()
        );
        assert_eq!(
            f.service.pipeline().output_path(),
            f.service.paths().runtime_config()
        );
    }

    /// Point the service at a binary that exists but always fails, i.e. a core
    /// that refuses every document. This is the case rollback exists for, and
    /// it needs no real core on the test machine.
    fn refuse_everything(f: &mut Fixture) {
        let mut s = f.service.settings().clone();
        s.core.binary = Some(PathBuf::from("/bin/false"));
        s.core.rollback_on_failure = true;
        f.service.set_settings(s);
    }

    /// Finding F19: `reload` replaced the reason a restart failed with
    /// `InvalidValue { field: "rollback" }` — "there are no snapshots to
    /// restore" — because `Pipeline::rollback` was called through `?` on a
    /// service whose first document had never been committed. `config generate
    /// --apply` therefore blamed rollback bookkeeping for a missing core and
    /// for a document the core rejected.
    #[tokio::test]
    async fn a_failed_reload_reports_the_real_cause_not_the_rollback_bookkeeping() {
        let mut f = fixture();
        f.seed();
        // A first apply: the document is written, but nothing was there
        // before it, so the pipeline has no snapshot to restore.
        let outcome = f.service.generate().unwrap();
        f.service.pipeline().commit(&outcome, false).unwrap();
        assert!(f.service.pipeline().snapshots().unwrap().is_empty());
        refuse_everything(&mut f);

        let err = f.service.reload(ReloadMode::Auto).await.unwrap_err();
        assert!(
            !matches!(
                &err,
                Error::InvalidValue {
                    field: "rollback",
                    ..
                }
            ),
            "bookkeeping must never displace the cause: {err:?}"
        );
        assert!(
            !err.to_string().contains("snapshot"),
            "nothing to roll back to is not a diagnosis: {err}"
        );
        assert!(
            matches!(err, Error::ProcessFailed { .. }),
            "the core's own refusal is the real cause: {err:?}"
        );
    }

    /// The complement: when a previous document *is* on disk, a rejected one is
    /// still undone, so the test above cannot pass by disabling rollback.
    #[tokio::test]
    async fn a_rejected_document_is_still_rolled_back_when_there_is_one_to_restore() {
        let mut f = fixture();
        let uid = f.seed();
        let first = f.service.generate().unwrap();
        f.service.pipeline().commit(&first, false).unwrap();

        {
            let store = f.service.store().unwrap();
            let item = store.get(&uid).unwrap().clone();
            store
                .write_document(&item, &BASE.replace("7890", "7891"))
                .unwrap();
            store.save().unwrap();
        }
        let second = f.service.generate().unwrap();
        f.service.pipeline().commit(&second, false).unwrap();
        assert_ne!(first.yaml, second.yaml, "the two documents must differ");

        refuse_everything(&mut f);
        // The restart cannot succeed with a core that refuses everything, but
        // the document that was working has to be back on disk.
        let _ = f.service.reload(ReloadMode::Auto).await;

        let restored = std::fs::read_to_string(f.service.paths().runtime_config()).unwrap();
        assert_eq!(
            restored, first.yaml,
            "the working document must be restored"
        );
    }
}
