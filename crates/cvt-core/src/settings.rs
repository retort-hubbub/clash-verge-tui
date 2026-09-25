//! Application settings.
//!
//! Everything user-tunable lives in one small document, `cvt.yaml` inside the
//! application home. Two rules keep it maintainable:
//!
//! * **Every field has a working default**, so a missing or partial file is
//!   never an error and a new setting never breaks an existing installation.
//! * **Unknown keys are preserved**, so a file written by a newer build
//!   survives a round-trip through an older one instead of being silently
//!   truncated.
//!
//! Settings are validated as a whole ([`Settings::validate`]) rather than
//! field by field, because the interesting mistakes are relationships: a
//! concurrency of zero, a test timeout that exceeds the core's `int16` limit.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Error, Result};
use crate::mihomo::types::LogLevel;
use crate::paths::AppPaths;

/// The largest millisecond timeout the core's delay endpoints accept.
///
/// `GET /proxies/{name}/delay` parses `timeout` as an `int16`, so anything
/// larger is rejected with `400 Body invalid`.
pub const MAX_TEST_TIMEOUT_MS: u32 = 32_767;

/// How the core process is managed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CoreSettings {
    /// Explicit path to the core binary; overrides discovery.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binary: Option<PathBuf>,
    /// Where the controller listens, forced over every profile.
    ///
    /// The control plane has to be settable from somewhere that a subscription
    /// update cannot overwrite, and a profile is exactly the wrong place for
    /// it: the base document is replaced wholesale whenever the subscription is
    /// refreshed. When this is set it wins over every profile, and profiles are
    /// not allowed to declare a control plane at all — see
    /// [`crate::enhance::pipeline::CONTROL_PLANE`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_controller: Option<String>,
    /// The controller's secret, forced over every profile.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
    /// Start the core when the application starts.
    pub auto_start: bool,
    /// Revert to the last snapshot when a new configuration fails to load.
    pub rollback_on_failure: bool,
}

impl Default for CoreSettings {
    fn default() -> Self {
        Self {
            binary: None,
            external_controller: None,
            secret: None,
            auto_start: false,
            rollback_on_failure: true,
        }
    }
}

/// Terminal presentation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UiSettings {
    /// Frame interval for redraws, in milliseconds.
    pub refresh_ms: u64,
    /// Minimum log level shown in the logs pane, and requested from the core.
    pub log_level: LogLevel,
    /// Show the help footer.
    pub show_footer: bool,
    /// Use colour; disable for a monochrome terminal or a pipe.
    pub color: bool,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            refresh_ms: 1000,
            log_level: LogLevel::Info,
            show_footer: true,
            color: true,
        }
    }
}

/// Latency testing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TestSettings {
    /// URL probed for latency. Must return 204 without a body to measure
    /// connection setup rather than transfer.
    pub url: String,
    /// Per-request timeout in milliseconds.
    pub timeout_ms: u32,
    /// How many nodes are tested at once.
    pub concurrency: usize,
    /// Accept this status expression; `*` accepts anything.
    pub expected_status: String,
    /// Discard results older than this, in seconds.
    pub cache_ttl_secs: u64,
}

impl Default for TestSettings {
    fn default() -> Self {
        Self {
            url: "https://www.gstatic.com/generate_204".to_owned(),
            timeout_ms: 5000,
            concurrency: 16,
            expected_status: "*".to_owned(),
            cache_ttl_secs: 1800,
        }
    }
}

/// Which live streams to open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StreamSettings {
    /// `/traffic`.
    pub traffic: bool,
    /// `/memory`.
    pub memory: bool,
    /// `/logs`.
    pub logs: bool,
    /// `/connections`.
    pub connections: bool,
}

impl Default for StreamSettings {
    fn default() -> Self {
        Self {
            traffic: true,
            memory: true,
            logs: true,
            connections: true,
        }
    }
}

impl From<StreamSettings> for crate::mihomo::stream::Selection {
    fn from(s: StreamSettings) -> Self {
        Self {
            traffic: s.traffic,
            memory: s.memory,
            logs: s.logs,
            connections: s.connections,
        }
    }
}

/// How the routing rules are applied to connections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UpdateSettings {
    /// Update due profiles when the application starts.
    pub update_on_start: bool,
    /// Close existing connections when a configuration is reloaded, so that
    /// connections do not keep using a node the new configuration removed.
    pub close_connections_on_apply: bool,
    /// Reload through the API when possible, restarting only as a fallback.
    pub prefer_hot_reload: bool,
}

impl Default for UpdateSettings {
    fn default() -> Self {
        Self {
            update_on_start: false,
            close_connections_on_apply: false,
            prefer_hot_reload: true,
        }
    }
}

/// What to do about the log files.
///
/// A core that runs for months writes to its log for months, and the file this
/// program points it at is one nobody would otherwise rotate. Rotation happens
/// when the core is *started*, which is the only moment it can be: the child
/// holds the file open while it runs, so renaming it underneath would leave a
/// live process writing into a file nothing will ever read again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LogSettings {
    /// Rotate a log once it is larger than this, in bytes. Zero disables it.
    pub max_size_bytes: u64,
    /// How many rotated files to keep per log, oldest dropped first.
    pub keep: usize,
    /// Delete rotated files older than this many days. Zero keeps them.
    pub keep_days: u64,
}

impl Default for LogSettings {
    fn default() -> Self {
        Self {
            // A megabyte is roughly a week of an ordinary session at the
            // default log level, and small enough that four of them are
            // nothing.
            max_size_bytes: 1024 * 1024,
            keep: 4,
            keep_days: 14,
        }
    }
}

/// Everything the user can tune.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Core process management.
    pub core: CoreSettings,
    /// Presentation.
    pub ui: UiSettings,
    /// Latency testing.
    pub test: TestSettings,
    /// Live streams.
    pub stream: StreamSettings,
    /// Configuration application.
    pub update: UpdateSettings,
    /// Log files.
    pub logs: LogSettings,
    /// Keys written by another version, preserved verbatim.
    #[serde(flatten)]
    pub other: serde_json::Map<String, Value>,
}

impl Settings {
    /// Load settings, falling back to defaults when the file is absent.
    ///
    /// A malformed file is an error: silently reverting to defaults would
    /// throw away the user's configuration without telling them.
    ///
    /// # Errors
    /// [`Error::Parse`] for malformed YAML, [`Error::Io`] when it is
    /// unreadable.
    pub fn load(paths: &AppPaths) -> Result<Self> {
        let path = paths.settings_file();
        if !path.is_file() {
            return Ok(Self::default());
        }
        let text = paths.read(&path)?;
        if text.trim().is_empty() {
            return Ok(Self::default());
        }
        serde_norway::from_str(&text).map_err(|e| Error::parse("settings", &path, e))
    }

    /// Write the settings atomically.
    ///
    /// # Errors
    /// [`Error::Serialize`], [`Error::Io`], or [`Error::InvalidValue`] when
    /// validation fails — an invalid file must never reach the disk.
    pub fn save(&self, paths: &AppPaths) -> Result<()> {
        self.validate()?;
        let yaml = serde_norway::to_string(self).map_err(|e| Error::serialize("settings", e))?;
        paths.write_atomic(&paths.settings_file(), &yaml)
    }

    /// Check every relationship that a per-field type cannot express.
    ///
    /// # Errors
    /// [`Error::InvalidValue`] naming the offending field.
    pub fn validate(&self) -> Result<()> {
        if self.ui.refresh_ms == 0 {
            return Err(Error::invalid(
                "ui.refresh_ms",
                "must be at least 1; a zero interval would spin the event loop",
            ));
        }
        if self.ui.refresh_ms > 60_000 {
            return Err(Error::invalid(
                "ui.refresh_ms",
                format!(
                    "{} ms is unreasonably slow; use at most 60000",
                    self.ui.refresh_ms
                ),
            ));
        }
        if self.logs.keep > 64 {
            return Err(Error::invalid(
                "logs.keep",
                format!(
                    "{} rotated files per log is more than anyone reads; use at most 64",
                    self.logs.keep
                ),
            ));
        }
        if self.test.concurrency == 0 {
            return Err(Error::invalid("test.concurrency", "must be at least 1"));
        }
        if self.test.concurrency > 512 {
            return Err(Error::invalid(
                "test.concurrency",
                format!(
                    "{} concurrent tests would exhaust file descriptors",
                    self.test.concurrency
                ),
            ));
        }
        if self.test.timeout_ms == 0 {
            return Err(Error::invalid("test.timeout_ms", "must be at least 1"));
        }
        if self.test.timeout_ms > MAX_TEST_TIMEOUT_MS {
            return Err(Error::invalid(
                "test.timeout_ms",
                format!("the core parses this as an int16; use at most {MAX_TEST_TIMEOUT_MS}"),
            ));
        }
        if self.test.url.trim().is_empty() {
            return Err(Error::invalid(
                "test.url",
                "a test URL is required; without one the core rejects every delay request",
            ));
        }
        if !self.test.url.starts_with("http://") && !self.test.url.starts_with("https://") {
            return Err(Error::invalid(
                "test.url",
                format!("`{}` is not an http(s) URL", self.test.url),
            ));
        }
        Ok(())
    }

    /// Whether every stream is disabled, which means the dashboard panes will
    /// stay empty and the UI should say so.
    #[must_use]
    pub fn streams_disabled(&self) -> bool {
        !self.stream.traffic && !self.stream.memory && !self.stream.logs && !self.stream.connections
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn paths() -> (TempDir, AppPaths) {
        let dir = TempDir::new().unwrap();
        let p = AppPaths::new(dir.path());
        p.ensure_dirs().unwrap();
        (dir, p)
    }

    #[test]
    fn a_missing_file_yields_working_defaults() {
        let (_d, p) = paths();
        let s = Settings::load(&p).unwrap();
        assert_eq!(s, Settings::default());
        assert!(s.validate().is_ok());
        assert!(
            !s.core.auto_start,
            "the core must never be started implicitly"
        );
        assert_eq!(s.test.concurrency, 16);
        assert!(s.test.timeout_ms <= MAX_TEST_TIMEOUT_MS);
        assert!(!s.streams_disabled());
    }

    #[test]
    fn an_empty_file_is_treated_as_absent() {
        let (_d, p) = paths();
        std::fs::write(p.settings_file(), "\n  \n").unwrap();
        assert_eq!(Settings::load(&p).unwrap(), Settings::default());
    }

    #[test]
    fn a_partial_file_fills_in_every_other_default() {
        let (_d, p) = paths();
        std::fs::write(p.settings_file(), "core:\n  auto_start: true\n").unwrap();
        let s = Settings::load(&p).unwrap();
        assert!(s.core.auto_start);
        assert_eq!(s.ui.refresh_ms, UiSettings::default().refresh_ms);
        assert_eq!(s.test.url, TestSettings::default().url);
    }

    #[test]
    fn save_and_reload_round_trips_including_unknown_keys() {
        let (_d, p) = paths();
        let mut s = Settings::default();
        s.ui.log_level = LogLevel::Debug;
        s.other
            .insert("from-the-future".to_owned(), Value::Bool(true));
        s.save(&p).unwrap();

        let reloaded = Settings::load(&p).unwrap();
        assert_eq!(reloaded.ui.log_level, LogLevel::Debug);
        assert_eq!(
            reloaded.other.get("from-the-future"),
            Some(&Value::Bool(true))
        );
    }

    #[test]
    fn a_malformed_file_is_an_error_not_a_silent_reset() {
        let (_d, p) = paths();
        std::fs::write(p.settings_file(), "ui: [not a mapping\n").unwrap();
        let err = Settings::load(&p).unwrap_err();
        assert!(matches!(err, Error::Parse { .. }), "{err:?}");
        assert!(p.settings_file().exists(), "the file must not be destroyed");
    }

    #[test]
    fn an_unknown_key_inside_a_known_section_is_rejected() {
        // `deny_unknown_fields` on the sections turns a typo into an error
        // rather than a setting that silently does nothing.
        let (_d, p) = paths();
        std::fs::write(p.settings_file(), "ui:\n  referesh_ms: 500\n").unwrap();
        assert!(Settings::load(&p).is_err());
    }

    #[test]
    fn validation_catches_every_relationship_a_type_cannot() {
        let bad = |mutate: fn(&mut Settings)| {
            let mut s = Settings::default();
            mutate(&mut s);
            s.validate()
        };

        assert!(bad(|s| s.ui.refresh_ms = 0).is_err());
        assert!(bad(|s| s.ui.refresh_ms = 60_001).is_err());
        assert!(bad(|s| s.test.concurrency = 0).is_err());
        assert!(bad(|s| s.test.concurrency = 513).is_err());
        assert!(bad(|s| s.test.timeout_ms = 0).is_err());
        // Exactly at the int16 limit is fine; one past it is not.
        assert!(bad(|s| s.test.timeout_ms = MAX_TEST_TIMEOUT_MS).is_ok());
        assert!(bad(|s| s.test.timeout_ms = MAX_TEST_TIMEOUT_MS + 1).is_err());
        assert!(bad(|s| s.test.url = String::new()).is_err());
        assert!(bad(|s| s.test.url = "ftp://example.com".into()).is_err());
    }

    #[test]
    fn validation_reports_the_offending_field() {
        let mut s = Settings::default();
        s.test.timeout_ms = 99_999;
        let err = s.validate().unwrap_err();
        assert!(err.to_string().contains("test.timeout_ms"), "{err}");
        assert!(
            err.to_string().contains("int16"),
            "the reason must be actionable: {err}"
        );
    }

    #[test]
    fn saving_something_invalid_is_refused() {
        let (_d, p) = paths();
        let mut s = Settings::default();
        s.test.concurrency = 0;
        assert!(s.save(&p).is_err());
        assert!(!p.settings_file().exists(), "nothing may be written");
    }

    #[test]
    fn stream_settings_convert_to_a_stream_selection() {
        let s = StreamSettings {
            traffic: false,
            memory: true,
            logs: false,
            connections: true,
        };
        let sel: crate::mihomo::stream::Selection = s.into();
        assert!(!sel.traffic);
        assert!(sel.memory);
        assert!(!sel.logs);
        assert!(sel.connections);

        let none = StreamSettings {
            traffic: false,
            memory: false,
            logs: false,
            connections: false,
        };
        assert!(
            Settings {
                stream: none,
                ..Settings::default()
            }
            .streams_disabled()
        );
    }

    #[test]
    fn log_levels_serialise_as_the_core_spells_them() {
        let (_d, p) = paths();
        let mut s = Settings::default();
        s.ui.log_level = LogLevel::Warning;
        s.save(&p).unwrap();
        let text = std::fs::read_to_string(p.settings_file()).unwrap();
        assert!(text.contains("log_level: warning"), "{text}");
        // `warn` is not a level the core accepts.
        assert!(!text.contains("log_level: warn\n"), "{text}");
    }
}
