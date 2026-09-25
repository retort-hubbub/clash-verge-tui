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
//! Steps 2 to 4 are [`Service::apply_with`], and the whole sequence is
//! exercisable in tests through a small injection point rather than a real
//! core.

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
/// How long a reloaded group may take to reappear, and how long a selection
/// may take to take effect.
///
/// Generous by the standards of a local API and small by the standards of a
/// person: the whole wait is bounded so a core that is reloading cannot make
/// an apply hang.
const REPLAY_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(1500);
const REPLAY_STEP: std::time::Duration = std::time::Duration::from_millis(50);

/// Wait until the core answers for a group again.
async fn wait_for_group(client: &Client, group: &str, deadline: std::time::Instant) -> bool {
    loop {
        if client.group(group).await.is_ok() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(REPLAY_STEP).await;
    }
}

/// Wait until the core reports the member that was asked for.
async fn confirm_selection(
    client: &Client,
    group: &str,
    member: &str,
    deadline: std::time::Instant,
) -> bool {
    loop {
        match client.group(group).await {
            Ok(view) if view.now.as_deref() == Some(member) => return true,
            // A group that has gone means this document no longer has it, and
            // the next attempt would fail the same way.
            Err(_) => return false,
            Ok(_) => {}
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
        Client::with_timeout(
            endpoint,
            std::time::Duration::from_millis(self.settings.ui.refresh_ms.max(1000) * 5),
        )
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
        self.supervisor()
            .locate(self.settings.core.binary.as_deref())
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
            return Err(Error::invalid(
                "runtime config",
                "no configuration has been generated yet; apply a profile first",
            ));
        }
        let text = self.paths.read(&config)?;
        let parsed = Config::from_yaml(&text)?;
        let report = crate::validate::check(&parsed);
        if !report.is_ok() {
            return Err(Error::Validation {
                problems: report.errors_iter().map(|d| d.message.clone()).collect(),
            });
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
        let selections_restored = self.restore_selections().await.unwrap_or(0);
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
        // One deadline for the whole replay, not one per group. A deadline per
        // group is a deadline that multiplies: a profile with twenty remembered
        // groups and a core that has lost them all would hold an apply for half
        // a minute, and the user would see a program that had stopped
        // responding rather than one that was waiting for something.
        let deadline = std::time::Instant::now() + REPLAY_TIMEOUT;
        let mut applied = 0;
        for selection in selections {
            if std::time::Instant::now() >= deadline {
                tracing::debug!("giving up on the rest of the remembered selections");
                break;
            }
            // A reload is applied by the core *in the background*: for a
            // moment the group is not there at all, and a selection made in
            // that window is silently discarded — which is exactly what the
            // first version of this did, so the choice appeared to be replayed
            // and was not. Wait for the group, choose, then confirm.
            if !wait_for_group(&client, &selection.name, deadline).await {
                tracing::debug!(group = %selection.name, "the group is not there to replay into");
                continue;
            }
            if let Err(error) = client.select(&selection.name, &selection.now).await {
                tracing::debug!(group = %selection.name, error = %error, "replay refused");
                continue;
            }
            if confirm_selection(&client, &selection.name, &selection.now, deadline).await {
                applied += 1;
            } else {
                tracing::debug!(
                    group = %selection.name,
                    member = %selection.now,
                    "the choice did not take; the configuration may have replaced the group"
                );
            }
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
                let reason = reason.short();
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

    /// Wait for the controller to answer after a restart.
    ///
    /// The core binds its listener a moment after the process starts, so a
    /// request immediately after `start` legitimately fails.
    ///
    /// # Errors
    /// [`Error::ControllerUnreachable`] when it never answers within the
    /// deadline.
    pub async fn wait_until_ready(&self) -> Result<()> {
        let client = self.client()?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut last = String::from("no attempt made");
        while std::time::Instant::now() < deadline {
            match client.version().await {
                Ok(v) => {
                    tracing::info!(version = %v.trimmed(), "core is up");
                    return Ok(());
                }
                Err(e) => last = e.short(),
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
        Err(Error::ControllerUnreachable {
            endpoint: self
                .endpoint()?
                .map_or_else(|| "unknown".to_owned(), |e| e.describe()),
            source: format!("the core did not become ready: {last}").into(),
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::profile::item::PrfItem;
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
