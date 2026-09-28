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
        self.client()?.wait_until_ready().await
    }
}
