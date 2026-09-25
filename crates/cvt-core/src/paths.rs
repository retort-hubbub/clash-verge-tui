//! On-disk layout of the application.
//!
//! Everything `clash-verge-tui` persists lives under a single *home* directory,
//! resolved in this order:
//!
//! 1. an explicit path passed by the caller (the `--home` flag),
//! 2. the `CVT_HOME` environment variable,
//! 3. the platform default data directory.
//!
//! The layout is deliberately close to `clash-verge-rev`'s so that an existing
//! installation can be imported (see [`AppPaths::detect_verge_homes`]).

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// Environment variable that overrides the home directory.
pub const HOME_ENV: &str = "CVT_HOME";

/// Directory name used for `clash-verge-tui`'s own data.
pub const APP_DIR_NAME: &str = "clash-verge-tui";

/// Bundle identifiers of `clash-verge-rev`, newest first.
///
/// Used by [`AppPaths::detect_verge_homes`] to offer an import path.
pub const VERGE_DIR_NAMES: &[&str] = &[
    "io.github.clash-verge-rev.clash-verge-rev",
    "io.github.clash-verge-rev.clash-verge",
    "clash-verge-rev",
    "clash-verge",
];

/// All paths the application reads from or writes to.
///
/// Construct one with [`AppPaths::resolve`] and pass it down; nothing else in
/// the crate reads environment variables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    home: PathBuf,
}

impl AppPaths {
    /// Resolve the home directory.
    ///
    /// `explicit` wins over `CVT_HOME`, which wins over the platform default.
    ///
    /// # Errors
    /// Returns [`Error::CoreUnavailable`] when no home can be determined, which
    /// only happens on platforms without a data directory and with no override.
    pub fn resolve(explicit: Option<&Path>) -> Result<Self> {
        if let Some(p) = explicit {
            return Ok(Self::new(p));
        }
        if let Some(from_env) = std::env::var_os(HOME_ENV).filter(|v| !v.is_empty()) {
            return Ok(Self::new(PathBuf::from(from_env)));
        }
        let base = dirs::data_dir().ok_or_else(|| Error::CoreUnavailable {
            reason: format!("could not determine a data directory; set {HOME_ENV} or pass --home"),
        })?;
        Ok(Self::new(base.join(APP_DIR_NAME)))
    }

    /// Wrap an already-known directory, without touching the environment.
    #[must_use]
    pub fn new(home: impl Into<PathBuf>) -> Self {
        Self { home: home.into() }
    }

    /// The home directory itself.
    #[must_use]
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// Application settings (`cvt.yaml`).
    #[must_use]
    pub fn settings_file(&self) -> PathBuf {
        self.home.join("cvt.yaml")
    }

    /// The profile index (`profiles.yaml`).
    #[must_use]
    pub fn profiles_index(&self) -> PathBuf {
        self.home.join("profiles.yaml")
    }

    /// Directory holding downloaded and local profiles.
    #[must_use]
    pub fn profiles_dir(&self) -> PathBuf {
        self.home.join("profiles")
    }

    /// Directory holding declarative override profiles.
    #[must_use]
    pub fn overrides_dir(&self) -> PathBuf {
        self.home.join("overrides")
    }

    /// Directory holding generated runtime artifacts.
    #[must_use]
    pub fn runtime_dir(&self) -> PathBuf {
        self.home.join("runtime")
    }

    /// The generated configuration handed to the core.
    #[must_use]
    pub fn runtime_config(&self) -> PathBuf {
        self.runtime_dir().join("config.yaml")
    }

    /// The previous good configuration, kept for rollback.
    #[must_use]
    pub fn runtime_config_previous(&self) -> PathBuf {
        self.runtime_dir().join("config.previous.yaml")
    }

    /// Directory holding the core binary and its geo databases.
    #[must_use]
    pub fn core_dir(&self) -> PathBuf {
        self.home.join("core")
    }

    /// Directory the core is launched with (`-d`), holding caches the core owns.
    #[must_use]
    pub fn core_work_dir(&self) -> PathBuf {
        self.core_dir().join("work")
    }

    /// Directory holding rotated snapshots of generated configs.
    #[must_use]
    pub fn snapshots_dir(&self) -> PathBuf {
        self.home.join("snapshots")
    }

    /// Where whole-home backups are kept.
    ///
    /// Separate from [`Self::snapshots_dir`] on purpose, and the names are
    /// close enough to be worth the sentence: a *snapshot* is one generated
    /// configuration, kept so that a bad apply can be undone in seconds. A
    /// *backup* is the profiles, the settings and the overrides — everything a
    /// user would have to recreate by hand — and is kept so that a mistake
    /// made a week ago can still be undone.
    #[must_use]
    pub fn backups_dir(&self) -> PathBuf {
        self.home.join("backups")
    }

    /// Directory holding log files.
    #[must_use]
    pub fn logs_dir(&self) -> PathBuf {
        self.home.join("logs")
    }

    /// Path of the core's own stdout/stderr capture file.
    #[must_use]
    pub fn core_log(&self) -> PathBuf {
        self.logs_dir().join("core.log")
    }

    /// Path of the application's own log file.
    #[must_use]
    pub fn app_log(&self) -> PathBuf {
        self.logs_dir().join("app.log")
    }

    /// Create every directory the application may write into.
    ///
    /// Idempotent. Call once during start-up.
    ///
    /// # Errors
    /// Propagates any filesystem failure as [`Error::Io`].
    pub fn ensure_dirs(&self) -> Result<()> {
        for dir in [
            self.home.clone(),
            self.profiles_dir(),
            self.overrides_dir(),
            self.runtime_dir(),
            self.core_dir(),
            self.core_work_dir(),
            self.snapshots_dir(),
            self.backups_dir(),
            self.logs_dir(),
        ] {
            std::fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
        }
        Ok(())
    }

    /// Read a file, mapping failures to [`Error::Io`].
    ///
    /// # Errors
    /// [`Error::Io`] if the file cannot be read.
    pub fn read(&self, path: &Path) -> Result<String> {
        std::fs::read_to_string(path).map_err(|e| Error::io(path, e))
    }

    /// Write a file atomically: write to a sibling temporary file, then rename.
    ///
    /// This guarantees a reader never observes a half-written config, which
    /// matters because the core hot-reloads these files.
    ///
    /// # Errors
    /// [`Error::Io`] if any step fails.
    pub fn write_atomic(&self, path: &Path, contents: &str) -> Result<()> {
        use std::io::Write as _;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        }
        let tmp = path.with_extension(format!(
            "{}.tmp",
            path.extension()
                .and_then(std::ffi::OsStr::to_str)
                .unwrap_or("out")
        ));
        {
            let mut f = std::fs::File::create(&tmp).map_err(|e| Error::io(&tmp, e))?;
            f.write_all(contents.as_bytes())
                .map_err(|e| Error::io(&tmp, e))?;
            f.sync_all().map_err(|e| Error::io(&tmp, e))?;
        }
        std::fs::rename(&tmp, path).map_err(|e| Error::io(path, e))?;
        Ok(())
    }

    /// Locations where an existing `clash-verge-rev` installation may live.
    ///
    /// The first entry that actually contains a `profiles.yaml` is the most
    /// likely import source, but every candidate is returned so the UI can let
    /// the user choose.
    #[must_use]
    pub fn detect_verge_homes() -> Vec<PathBuf> {
        let mut roots: Vec<PathBuf> = Vec::new();
        if let Some(d) = dirs::data_dir() {
            roots.push(d);
        }
        if let Some(d) = dirs::config_dir() {
            roots.push(d);
        }
        #[cfg(target_os = "macos")]
        if let Some(home) = dirs::home_dir() {
            roots.push(home.join("Library/Application Support"));
        }
        let mut out = Vec::new();
        for root in roots {
            for name in VERGE_DIR_NAMES {
                let candidate = root.join(name);
                if candidate.join("profiles.yaml").is_file() {
                    out.push(candidate);
                }
            }
        }
        out.dedup();
        out
    }

    /// `true` when this home already holds a profile index.
    #[must_use]
    pub fn is_initialised(&self) -> bool {
        self.profiles_index().is_file()
    }
}
