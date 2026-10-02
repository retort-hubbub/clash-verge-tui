//! Configuration deployment, readiness and rollback.

use super::{ApplyReport, Config, Error, ReloadMode, ReloadOutcome, Result, Service};

impl Service {
    /// Generate, write, and hand the configuration to the core.
    ///
    /// # Errors
    /// [`Error::Validation`] when the result has errors and `force` is not
    /// set; otherwise propagates generation or commit failures.
    pub async fn apply(&self, force: bool, mode: ReloadMode) -> Result<ApplyReport> {
        let outcome = self.generate()?;
        let pipeline = self.pipeline();
        if mode == ReloadMode::HotReload && self.controller_changes(&outcome.config) {
            return Err(Error::invalid(
                "reload mode",
                "controller address or secret changes require a restart; use auto or restart",
            ));
        }
        let restarting = mode == ReloadMode::Restart
            || (mode == ReloadMode::Auto && self.controller_changes(&outcome.config));
        if restarting && self.core_status().is_running() {
            // The old process still owns listeners, including ports a new
            // configuration may assign to a different listener class.
            // Syntax checks precede commit; resource checks follow its exit.
            self.validate_candidate_syntax(&outcome)?;
        } else {
            self.validate_candidate(&outcome)?;
        }
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

    fn controller_changes(&self, config: &Config) -> bool {
        if !self.core_status().is_running() {
            return false;
        }
        match self.supervisor().controller_endpoint() {
            Some(current) => Some(current) != super::Endpoint::from_config(config),
            // Old records cannot establish which controller belongs to the
            // process. Restart once rather than connect to an inferred address.
            None => true,
        }
    }

    /// Validate the candidate without overwriting the running configuration.
    ///
    /// # Errors
    /// Returns local conflicts or Mihomo parser failures before commit.
    pub fn validate_candidate(&self, outcome: &crate::enhance::pipeline::Outcome) -> Result<()> {
        self.validate_environment(&outcome.config)?;
        self.validate_candidate_syntax(outcome)
    }

    fn validate_candidate_syntax(&self, outcome: &crate::enhance::pipeline::Outcome) -> Result<()> {
        if let Some(binary) = self.core_binary() {
            use std::io::Write as _;
            let directory = self.paths.runtime_dir();
            std::fs::create_dir_all(&directory).map_err(|error| Error::io(&directory, error))?;
            let mut candidate = tempfile::Builder::new()
                .prefix("preflight-")
                .suffix(".yaml")
                .tempfile_in(&directory)
                .map_err(|error| Error::io(&directory, error))?;
            candidate
                .write_all(outcome.yaml.as_bytes())
                .map_err(|error| Error::io(candidate.path(), error))?;
            self.supervisor()
                .validate_config(&binary, candidate.path())?;
        }
        Ok(())
    }

    /// Hand the already-written runtime configuration to the core.
    ///
    /// Tries the requested reload strategy and restores a snapshot when rollback
    /// is enabled and the failure is recoverable. Recovery writes runtime state.
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
        let was_running = self.core_status().is_running();
        let result = match mode {
            ReloadMode::Restart => self.restart_with_rollback().await,
            ReloadMode::HotReload => self.hot_reload().await.map(|()| ReloadOutcome::HotReloaded),
            ReloadMode::Auto
                if self.controller_changes(&Config::from_yaml(
                    &self.paths.read(&self.paths.runtime_config())?,
                )?) =>
            {
                self.restart_with_rollback().await
            }
            ReloadMode::Auto => match self.hot_reload().await {
                Ok(()) => Ok(ReloadOutcome::HotReloaded),
                Err(reason) => {
                    tracing::warn!(error = %reason, "hot reload failed; restarting the managed core");
                    self.restart_with_rollback().await
                }
            },
        };
        match result {
            Ok(outcome) => Ok(outcome),
            Err(error)
                if self.settings.core.rollback_on_failure
                    && !self.pipeline().snapshots()?.is_empty() =>
            {
                let reason = error.to_string();
                let snapshot = self.pipeline().rollback()?;
                let recovery = async {
                    // A failed apply must not turn a previously stopped core
                    // into a running one merely to restore its file.
                    if !was_running {
                        self.stop_core()?;
                        return Ok(());
                    }
                    // The old process may still be healthy after an API refusal.
                    // Restore its document without discarding connections first.
                    if self.hot_reload().await.is_err() {
                        self.restart_core()?;
                    }
                    self.wait_until_ready().await
                }
                .await;
                recovery.map_err(|failure| Error::invalid("rollback", format!(
                    "apply failed: {reason}; previous configuration restored on disk, but recovery failed: {failure}")))?;
                Ok(ReloadOutcome::RolledBack { reason, snapshot })
            }
            Err(error) => Err(error),
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
        let endpoint = self.supervisor().controller_endpoint().ok_or_else(||
            Error::invalid("core identity", "the running core has no recorded controller identity; restart it once before hot reload"))?;
        let client = super::Client::new(endpoint)?;
        if self.settings.update.close_connections_on_apply {
            // Best effort: a core that refuses this is still reloadable.
            let _ = client.close_all_connections().await;
        }
        self.supervisor().begin_reload()?;
        client
            .reload_configs(Some(&self.paths.runtime_config()), None, true)
            .await?;
        self.supervisor().finish_reload()?;
        self.wait_until_ready().await
    }

    /// Restart the core, waiting for the API to come back.
    async fn restart_with_rollback(&self) -> Result<ReloadOutcome> {
        let pid = self.restart_core()?;
        if let Err(error) = self.wait_until_ready().await {
            if matches!(self.core_status(), super::CoreStatus::Running { pid: current, .. } if current == pid)
            {
                let _ = self.stop_core();
            }
            return Err(error);
        }
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
    pub(super) async fn wait_for_document(&self, config: &Config) {
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
        self.client()?.wait_until_ready().await?;
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        self.supervisor().check_health()
    }
}
