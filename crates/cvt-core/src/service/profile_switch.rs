//! Transactional profile selection and exact runtime recovery.

use super::{Error, ReloadMode, Result, Service};

impl Service {
    /// Validate and deploy a proposed base before persisting its selection.
    /// Callers serialize this operation with other profile and lifecycle edits.
    ///
    /// # Errors
    /// Rejected deployments leave the profile index unchanged. Recovery errors
    /// explicitly report when the previous runtime could not be resumed.
    pub async fn switch_profile(&self, uid: &str, mode: ReloadMode) -> Result<String> {
        let mut candidate = self.store()?;
        candidate.set_current(uid)?;
        let name = candidate
            .current()
            .expect("set_current validated the profile")
            .name
            .clone();
        let pipeline = self.pipeline();
        let outcome = pipeline.generate(&candidate)?;
        let was_running = self.core_status().is_running();
        if was_running && mode == ReloadMode::HotReload && self.controller_changes(&outcome.config)
        {
            return Err(Error::invalid(
                "reload mode",
                "controller changes require a restart",
            ));
        }
        let restarting = was_running
            && (mode == ReloadMode::Restart
                || (mode == ReloadMode::Auto && self.controller_changes(&outcome.config)));
        if restarting {
            self.validate_candidate_syntax(&outcome)?;
        } else {
            self.validate_candidate(&outcome)?;
        }
        let runtime = self.paths.runtime_config();
        let previous = if runtime.is_file() {
            Some(self.paths.read(&runtime)?)
        } else {
            None
        };
        if was_running && previous.is_none() {
            return Err(Error::invalid(
                "runtime config",
                "the running core's configuration is missing; stop it before switching profiles",
            ));
        }
        pipeline.commit(&outcome, false)?;
        let result = async {
            if was_running {
                // Recovery uses the exact document captured for this operation,
                // never an unrelated historic snapshot or config.previous.yaml.
                self.reload_with_recovery(mode, false).await?;
                self.wait_for_document(&outcome.config).await;
            }
            candidate.save()
        }
        .await;
        if let Err(error) = result {
            let recovery = self
                .restore_profile_runtime(previous.as_deref(), was_running)
                .await;
            return match recovery {
                Ok(()) => Err(error),
                Err(failure) => Err(Error::invalid(
                    "profile switch",
                    format!(
                        "{error}; the previous profile remains selected, but runtime recovery failed: {failure}"
                    ),
                )),
            };
        }
        if was_running {
            let _ = self.restore_selections().await;
        }
        Ok(name)
    }

    async fn restore_profile_runtime(&self, previous: Option<&str>, running: bool) -> Result<()> {
        if running {
            self.stop_core()?;
        }
        let runtime = self.paths.runtime_config();
        if let Some(text) = previous {
            self.paths.write_atomic(&runtime, text)?;
        } else {
            match std::fs::remove_file(&runtime) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(Error::io(&runtime, error)),
            }
        }
        if running {
            self.start_runtime_core()?;
            self.wait_until_ready().await?;
            let _ = self.restore_selections().await;
        }
        Ok(())
    }
}
