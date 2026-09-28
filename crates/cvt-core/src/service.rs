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
mod deployment;
mod lifecycle;
mod selection;

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests;

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
        Pipeline::new(self.paths.clone())
            .with_control_plane(
                self.settings.core.external_controller.as_deref(),
                self.settings.core.secret.as_deref(),
            )
            .with_tun_enabled(self.settings.core.tun_enabled)
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
}
