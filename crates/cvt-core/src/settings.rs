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

/// The most nodes this program will measure at once.
///
/// Shared with the command line rather than written twice. The flag and the
/// setting are the same number in two places, and a ceiling that only guards
/// the one in the settings file is one somebody can walk around by typing it.
pub const MAX_TEST_CONCURRENCY: usize = 512;

/// How the core process is managed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CoreSettings {
    /// Explicit path to the core binary; overrides discovery.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binary: Option<PathBuf>,
    /// Where the controller listens, forced over every profile.
    ///
    /// Subscription documents never supply management endpoints. Missing values
    /// are migrated to a loopback controller when settings are loaded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_controller: Option<String>,
    /// The controller's secret, forced over every profile.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
    /// Start the core when the application starts.
    pub auto_start: bool,
    /// Revert to the last snapshot when a new configuration fails to load.
    pub rollback_on_failure: bool,
    /// Prefer the managed core in the application data directory over local/system binaries.
    #[serde(default = "default_true")]
    pub use_managed: bool,
    /// Start the core after login using the selected user session manager.
    pub login_autostart: LoginAutostart,
    /// Override the selected profile's TUN switch. `None` follows the profile.
    pub tun_enabled: Option<bool>,
    /// User-approved local DNS listener, applied after subscription enhancements.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dns_listen: Option<String>,
}

/// Supported login startup managers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LoginAutostart {
    /// Do not register a login startup entry.
    #[default]
    Off,
    /// A systemd user service.
    Systemd,
    /// XDG desktop autostart, limited to KDE Plasma.
    Kde,
    /// XDG desktop autostart, limited to GNOME.
    Gnome,
}

impl LoginAutostart {
    /// Stable setting value shown in the terminal.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Systemd => "systemd",
            Self::Kde => "KDE",
            Self::Gnome => "GNOME",
        }
    }

    /// Cycle through supported choices; validation happens at installation.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Off => Self::Systemd,
            Self::Systemd => Self::Kde,
            Self::Kde => Self::Gnome,
            Self::Gnome => Self::Off,
        }
    }
}

const fn default_true() -> bool {
    true
}

impl Default for CoreSettings {
    fn default() -> Self {
        Self {
            binary: None,
            external_controller: Some("127.0.0.1:9090".to_owned()),
            secret: None,
            auto_start: false,
            rollback_on_failure: true,
            use_managed: true,
            login_autostart: LoginAutostart::Off,
            tun_enabled: None,
            dns_listen: None,
        }
    }
}

/// Supported terminal interface languages.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Language {
    /// English interface text.
    #[default]
    #[serde(rename = "en")]
    English,
    /// Simplified Chinese interface text.
    #[serde(rename = "zh-CN", alias = "zh")]
    Chinese,
}

impl Language {
    /// Stable value stored in `cvt.yaml`.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::Chinese => "zh-CN",
        }
    }

    /// Name displayed in the language's own writing system.
    #[must_use]
    pub const fn native_name(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::Chinese => "简体中文",
        }
    }

    /// The other available language.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::English => Self::Chinese,
            Self::Chinese => Self::English,
        }
    }
}

/// Terminal presentation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UiSettings {
    /// Language of the terminal interface. CLI output remains machine stable.
    pub language: Language,
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
            language: Language::default(),
            refresh_ms: 1000,
            log_level: LogLevel::Info,
            show_footer: true,
            color: true,
        }
    }
}

/// One URL a node can be tested against.
///
/// `default` on both fields so an entry that names only one of them is a
/// validation error rather than a parse error: the message can then say which
/// entry is wrong, which a parse failure cannot.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TestTarget {
    /// The name to ask for it by, as in `--url google`.
    pub name: String,
    /// The URL to fetch.
    pub url: String,
}

impl TestTarget {
    /// A target, for a test or a default.
    #[must_use]
    pub fn new(name: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            url: url.into(),
        }
    }
}

/// What `test.urls` is by default.
///
/// The three places a person actually wants to know about: a site that is
/// reachable everywhere and useful as a baseline, one that is the reason most
/// people are here, and one that is blocked often enough to be worth its own
/// line. A delay to *anything* only says a socket opened; which thing it opened
/// to is the part that decides whether a node is any use.
fn default_targets() -> Vec<TestTarget> {
    vec![
        TestTarget::new("google", "https://www.gstatic.com/generate_204"),
        TestTarget::new("github", "https://github.com/robots.txt"),
        TestTarget::new("youtube", "https://www.youtube.com/robots.txt"),
    ]
}

impl TestSettings {
    /// The URL for a name, or the literal string when it is not a name.
    ///
    /// `--url google` and `--url https://…` are the same flag, and the reason is
    /// that both are things a person types. A name that does not exist is *not*
    /// silently treated as a URL: it would be fetched, fail, and look like a
    /// node problem rather than a typo.
    #[must_use]
    pub fn resolve(&self, given: &str) -> Option<&str> {
        // A URL is a URL, whatever a target happens to be called. Without this
        // a target *named* `https://example.com/` made `--url https://example.com/`
        // fetch the target's URL instead — a name that shadows the thing it
        // looks like, which is the one way a name could be used to fetch
        // somewhere the user did not ask for.
        if given.starts_with("http://") || given.starts_with("https://") {
            return None;
        }
        self.urls
            .iter()
            .find(|target| target.name == given)
            .map(|target| target.url.as_str())
    }
}

/// Latency testing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TestSettings {
    /// URL probed for latency. Must return 204 without a body to measure
    /// connection setup rather than transfer.
    pub url: String,
    /// Named URLs, for asking about a particular site rather than a socket.
    #[serde(default = "default_targets")]
    pub urls: Vec<TestTarget>,
    /// Per-request timeout in milliseconds.
    pub timeout_ms: u32,
    /// How many nodes are tested at once.
    pub concurrency: usize,
    /// Accept this status expression; `*` accepts anything.
    pub expected_status: String,
}

impl Default for TestSettings {
    fn default() -> Self {
        Self {
            url: "https://www.gstatic.com/generate_204".to_owned(),
            urls: default_targets(),
            timeout_ms: 5000,
            concurrency: 16,
            expected_status: "*".to_owned(),
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
        #[cfg(unix)]
        let _lock = paths.lock_settings()?;
        let path = paths.settings_file();
        let mut settings: Self = if path.is_file() {
            let text = paths.read(&path)?;
            if text.trim().is_empty() {
                Self::default()
            } else {
                serde_norway::from_str(&text).map_err(|e| Error::parse("settings", &path, e))?
            }
        } else {
            Self::default()
        };
        let controller_migrated = if settings
            .core
            .external_controller
            .as_deref()
            .is_none_or(|s| s.trim().is_empty())
        {
            settings.core.external_controller = Some("127.0.0.1:9090".to_owned());
            true
        } else {
            false
        };
        // None means uninitialised; Some("") is an explicit unauthenticated
        // choice and must survive restart instead of being silently changed.
        let secret_migrated = if settings.core.secret.is_none() {
            let mut random = [0_u8; 32];
            getrandom::fill(&mut random).map_err(|error| {
                Error::invalid(
                    "controller secret",
                    format!("OS randomness unavailable: {error}"),
                )
            })?;
            use std::fmt::Write as _;
            let mut secret = String::with_capacity(64);
            for byte in random {
                let _ = write!(secret, "{byte:02x}");
            }
            settings.core.secret = Some(secret);
            true
        } else {
            false
        };
        let migrated = !path.is_file() || controller_migrated || secret_migrated;
        // The same checks `save` runs, run on the way in. Otherwise the cap is
        // enforced only against values this program wrote, and the documented
        // way to change one — editing `cvt.yaml` — bypasses it: a `keep` of a
        // hundred million is accepted by the loader and then costs minutes of
        // syscalls on every start, on both logs.
        settings.validate()?;
        if migrated {
            paths.ensure_dirs()?;
            settings.save(paths)?;
        }
        Ok(settings)
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
        if let Some(address) = &self.core.dns_listen {
            let socket = address.parse::<std::net::SocketAddr>().map_err(|_| {
                Error::invalid("core.dns_listen", "use a loopback IP address and port")
            })?;
            if !socket.ip().is_loopback() || socket.port() == 0 {
                return Err(Error::invalid(
                    "core.dns_listen",
                    "use a loopback IP address and nonzero port",
                ));
            }
        }
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
        for (index, target) in self.test.urls.iter().enumerate() {
            if target.name.trim().is_empty() {
                return Err(Error::invalid(
                    "settings",
                    format!("test.urls[{index}] has an empty name, so it cannot be asked for"),
                ));
            }
            if self
                .test
                .urls
                .iter()
                .take(index)
                .any(|earlier| earlier.name == target.name)
            {
                return Err(Error::invalid(
                    "settings",
                    format!("test.urls has two entries called `{}`", target.name),
                ));
            }
            if !target.url.starts_with("http://") && !target.url.starts_with("https://") {
                return Err(Error::invalid(
                    "settings",
                    format!(
                        "test.urls `{}` is `{}`, which is not an http(s) URL",
                        target.name, target.url
                    ),
                ));
            }
        }
        if self.test.concurrency == 0 {
            return Err(Error::invalid("test.concurrency", "must be at least 1"));
        }
        // `core.external_controller` is forced over every profile, so a value
        // the generator refuses means *every* configuration is refused — with
        // `E-CONTROLLER-FORMAT`, from a file the settings accepted. Every other
        // relationship the settings carry is checked here; this one was not
        // checked at all.
        if let Some(controller) = &self.core.external_controller {
            let trimmed = controller.trim();
            // The generator's own rule, not a second one written here: it
            // refuses a controller without a colon with `E-CONTROLLER-FORMAT`,
            // and a stricter check in the settings would refuse values the
            // generator accepts — the mistake this project has recorded more
            // than any other.
            if !trimmed.is_empty() && !trimmed.contains(':') {
                return Err(Error::invalid(
                    "settings",
                    format!(
                        "core.external_controller is `{controller}`, which is not \
                         `host:port`; the core would refuse every configuration \
                         generated from it"
                    ),
                ));
            }
        }
        if self.test.concurrency > MAX_TEST_CONCURRENCY {
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
    fn a_test_url_can_be_asked_for_by_name() {
        let settings = Settings::default();
        assert_eq!(
            settings.test.resolve("google"),
            Some("https://www.gstatic.com/generate_204"),
            "the default list has the three places a person actually asks about"
        );
        // A name that is not in the list is not a URL, and is not silently
        // treated as one — the caller refuses it and lists what there is.
        assert_eq!(settings.test.resolve("googl"), None);
        assert_eq!(settings.test.resolve("https://example.com/"), None);
    }

    #[test]
    fn the_concurrency_ceiling_is_one_number() {
        // The flag and the setting are the same limit, so the ceiling is
        // exported and both use it. A test either side of it: `--concurrency`
        // used to be passed straight through while the same number in
        // `cvt.yaml` was refused.
        let mut settings = Settings::default();
        settings.test.concurrency = MAX_TEST_CONCURRENCY;
        assert!(settings.validate().is_ok(), "the ceiling itself is allowed");

        settings.test.concurrency = MAX_TEST_CONCURRENCY + 1;
        let error = settings.validate().unwrap_err().to_string();
        assert!(error.contains("file descriptors"), "{error}");
    }

    #[test]
    fn a_target_name_cannot_shadow_a_url() {
        let mut settings = Settings::default();
        settings.test.urls = vec![
            TestTarget::new("mirror", "https://mirror.example/"),
            TestTarget::new("https://example.com/", "https://attacker.example/"),
        ];
        // The shape is accepted, so `resolve` is what has to tell them apart.
        assert!(settings.validate().is_ok());
        assert_eq!(
            settings.test.resolve("https://example.com/"),
            None,
            "a URL the user typed is a URL, whatever a target is called"
        );
        assert_eq!(
            settings.test.resolve("mirror"),
            Some("https://mirror.example/")
        );
    }

    #[test]
    fn a_target_list_that_cannot_be_asked_for_is_refused() {
        let mut settings = Settings::default();
        settings.test.urls = vec![TestTarget::new("", "https://x.example/")];
        assert!(
            settings.validate().is_err(),
            "an empty name is a target nobody can ask for"
        );

        settings.test.urls = vec![
            TestTarget::new("a", "https://x.example/"),
            TestTarget::new("a", "https://y.example/"),
        ];
        let error = settings.validate().unwrap_err().to_string();
        assert!(error.contains("two entries"), "{error}");

        settings.test.urls = vec![TestTarget::new("a", "ftp://x.example/")];
        let error = settings.validate().unwrap_err().to_string();
        assert!(error.contains("not an http(s) URL"), "{error}");
    }

    #[test]
    fn a_missing_file_yields_working_defaults() {
        let (_d, p) = paths();
        let s = Settings::load(&p).unwrap();
        let secret = s.core.secret.as_deref().unwrap();
        assert_eq!(secret.len(), 64);
        assert!(secret.bytes().all(|b| b.is_ascii_hexdigit()));
        let mut expected = Settings::default();
        expected.core.secret = s.core.secret.clone();
        assert_eq!(s, expected);
        assert_eq!(
            Settings::load(&p).unwrap(),
            s,
            "generated credentials persist"
        );
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
        let loaded = Settings::load(&p).unwrap();
        assert_eq!(
            loaded.core.external_controller.as_deref(),
            Some("127.0.0.1:9090")
        );
        assert_eq!(loaded.core.secret.as_deref().unwrap().len(), 64);
        assert_eq!(Settings::load(&p).unwrap(), loaded);
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
    fn language_defaults_for_old_files_and_survives_a_save() {
        let (_d, paths) = paths();
        std::fs::write(paths.settings_file(), "ui:\n  color: false\n").unwrap();
        let mut settings = Settings::load(&paths).unwrap();
        assert_eq!(settings.ui.language, Language::English);
        settings.ui.language = Language::Chinese;
        settings.save(&paths).unwrap();
        assert_eq!(
            Settings::load(&paths).unwrap().ui.language,
            Language::Chinese
        );
        assert!(
            std::fs::read_to_string(paths.settings_file())
                .unwrap()
                .contains("zh-CN")
        );
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
