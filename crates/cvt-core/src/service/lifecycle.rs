//! Process startup, shutdown and log rotation behind the Service facade.

use super::{Config, CoreStatus, Error, Result, Service, Supervisor};

impl Service {
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
        if let CoreStatus::Running { pid, .. } = self.core_status() {
            return Err(Error::Unsupported(format!(
                "the core is already running as pid {pid}; stop it first"
            )));
        }
        if self.core_binary().is_none() {
            return Err(Error::CoreUnavailable {
                reason: "no mihomo binary found; install the managed core or set CVT_CORE"
                    .to_owned(),
            });
        }
        let config = self.paths.runtime_config();
        // A runtime with valid controller settings may still belong to an old
        // profile. Never silently launch it for a newly selected broken base.
        if self.store()?.current().is_some() || !config.is_file() {
            let outcome = self.generate()?;
            if !outcome.is_applicable() {
                return Err(Error::Validation {
                    problems: outcome
                        .report
                        .errors_iter()
                        .map(|d| d.message.clone())
                        .collect(),
                });
            }
            self.validate_candidate(&outcome)?;
            let existing = if config.is_file() {
                Some(Config::from_yaml(&self.paths.read(&config)?)?)
            } else {
                None
            };
            if existing.as_ref() != Some(&outcome.config) {
                self.pipeline().commit(&outcome, false)?;
            }
        }
        self.start_runtime_core()
    }

    /// Launch the explicitly deployed document, including a pending profile
    /// transaction or an exact recovery copy. Do not regenerate from the index.
    pub(super) fn start_runtime_core(&self) -> Result<u32> {
        let supervisor = self.supervisor();
        let binary = self.core_binary().ok_or_else(|| Error::CoreUnavailable {
            reason: format!(
                "no mihomo binary found; put one at {} or set {}",
                self.paths.core_dir().join("mihomo").display(),
                crate::mihomo::supervisor::CORE_ENV
            ),
        })?;
        let config = self.paths.runtime_config();
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
        self.validate_environment(&parsed)?;
        supervisor.validate_config(&binary, &config)?;
        // Logs are rotated here or never: the child holds its log open for as
        // long as it runs, so this is the only moment either file can be moved
        // without a live process writing into a file nobody will read.
        self.rotate_logs(&supervisor);
        let pid = supervisor.start(&binary, &config)?;
        std::thread::sleep(std::time::Duration::from_millis(250));
        if let Err(error) = supervisor
            .check_health()
            .and_then(|()| supervisor.synchronize_dns(&parsed))
        {
            let _ = supervisor.stop();
            return Err(error);
        }
        Ok(pid)
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
        let config = self.paths.runtime_config();
        if config.is_file() {
            let existing = Config::from_yaml(&self.paths.read(&config)?)?;
            if (cfg!(target_os = "linux")
                && existing.tun_enabled()
                && existing
                    .get("tun")
                    .and_then(|tun| tun.get("device"))
                    .and_then(serde_json::Value::as_str)
                    != Some(crate::mihomo::resolver::DEVICE))
                || self
                    .settings
                    .core
                    .dns_listen
                    .as_deref()
                    .is_some_and(|address| {
                        existing
                            .get("dns")
                            .and_then(|dns| dns.get("listen"))
                            .and_then(serde_json::Value::as_str)
                            != Some(address)
                    })
            {
                let outcome = self.generate()?;
                self.validate_candidate_syntax(&outcome)?;
                self.pipeline().commit(&outcome, false)?;
            }
        }
        if config.is_file() {
            let parsed = Config::from_yaml(&self.paths.read(&config)?)?;
            // Syntax is safe to check while the old core owns its sockets.
            // Resource availability is checked by start_core after stop has
            // confirmed exit; probing here can reject our own listeners.
            let report = crate::validate::check(&parsed);
            if !report.is_ok() {
                return Err(Error::Validation {
                    problems: report.errors_iter().map(|d| d.message.clone()).collect(),
                });
            }
            let binary = self.core_binary().ok_or_else(|| Error::CoreUnavailable {
                reason: "no Mihomo binary found".to_owned(),
            })?;
            self.supervisor().validate_config(&binary, &config)?;
        }
        self.stop_core()?;
        self.start_runtime_core()
    }
}
