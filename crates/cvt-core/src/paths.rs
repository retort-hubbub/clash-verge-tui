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
/// Construct one with [`AppPaths::resolve`] and pass it down. Tests can use
/// [`AppPaths::new`] to select a temporary home without changing the environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    home: PathBuf,
}

impl AppPaths {
    /// Restrict newly created files in this process and its child programs.
    /// Call once at executable startup, before application files are opened.
    pub fn set_private_creation_mask() {
        #[cfg(unix)]
        rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o077));
    }

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
            Self::ensure_private_dir(&dir)?;
        }
        Self::protect_existing_tree(&self.home)?;
        Ok(())
    }

    /// Create an application directory with owner-only access on Unix.
    pub(crate) fn ensure_private_dir(path: &Path) -> Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(path)
                .map_err(|e| Error::io(path, e))?;
            let meta = std::fs::symlink_metadata(path).map_err(|e| Error::io(path, e))?;
            if !meta.is_dir() || meta.uid() != rustix::process::getuid().as_raw() {
                return Err(Error::invalid(
                    "application directory",
                    format!(
                        "{} must be a real directory owned by the current user",
                        path.display()
                    ),
                ));
            }
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| Error::io(path, e))?;
        }
        #[cfg(not(unix))]
        std::fs::create_dir_all(path).map_err(|e| Error::io(path, e))?;
        Ok(())
    }

    fn protect_existing_tree(directory: &Path) -> Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
            for entry in std::fs::read_dir(directory).map_err(|e| Error::io(directory, e))? {
                let path = entry.map_err(|e| Error::io(directory, e))?.path();
                let meta = match std::fs::symlink_metadata(&path) {
                    Ok(meta) => meta,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(Error::io(&path, error)),
                };
                // Never change permissions through a link into another home.
                if meta.file_type().is_symlink() {
                    continue;
                }
                if meta.is_dir() {
                    Self::ensure_private_dir(&path)?;
                    Self::protect_existing_tree(&path)?;
                } else if meta.is_file() {
                    if meta.uid() != rustix::process::getuid().as_raw() || meta.nlink() > 1 {
                        return Err(Error::invalid(
                            "private state",
                            format!(
                                "{} must be an owned file without hard links",
                                path.display()
                            ),
                        ));
                    }
                    let mode = if meta.permissions().mode() & 0o111 != 0 {
                        0o700
                    } else {
                        0o600
                    };
                    if meta.permissions().mode() & 0o7777 != mode {
                        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))
                            .map_err(|e| Error::io(&path, e))?;
                    }
                }
            }
        }
        #[cfg(not(unix))]
        let _ = directory;
        Ok(())
    }

    /// Serialise initial controller-secret migration across application processes.
    #[cfg(unix)]
    pub(crate) fn lock_settings(&self) -> Result<std::fs::File> {
        use std::os::unix::fs::OpenOptionsExt as _;
        Self::ensure_private_dir(self.home())?;
        let path = self.home.join("settings.lock");
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits().cast_signed())
            .open(&path)
            .map_err(|e| Error::io(&path, e))?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => return Ok(file),
                Err(rustix::io::Errno::WOULDBLOCK) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(error) => return Err(Error::io(&path, std::io::Error::from(error))),
            }
        }
    }

    /// Read a file, mapping failures to [`Error::Io`].
    ///
    /// # Errors
    /// [`Error::Io`] if the file cannot be read.
    pub fn read(&self, path: &Path) -> Result<String> {
        if path.starts_with(self.home())
            && std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
        {
            return Err(Error::invalid(
                "private state",
                format!("{} must not be a symbolic link", path.display()),
            ));
        }
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

        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            Self::ensure_private_dir(parent)?;
        }
        let directory = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        // Unique, exclusively created files prevent collisions and symlink
        // attacks on predictable .tmp names. tempfile defaults to mode 0600,
        // keeping subscription credentials and controller secrets private.
        let mut temporary =
            tempfile::NamedTempFile::new_in(directory).map_err(|error| Error::io(path, error))?;
        temporary
            .write_all(contents.as_bytes())
            .map_err(|error| Error::io(path, error))?;
        temporary
            .as_file()
            .sync_all()
            .map_err(|error| Error::io(path, error))?;
        temporary
            .persist(path)
            .map_err(|error| Error::io(path, error.error))?;
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

#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used)]
mod atomic_write_tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    #[test]
    fn atomic_state_is_private_and_does_not_follow_predictable_temp_links() {
        let directory = tempfile::tempdir().unwrap();
        let paths = AppPaths::new(directory.path());
        let target = directory.path().join("state.yaml");
        let other = directory.path().join("unrelated");
        std::fs::write(&other, "keep").unwrap();
        symlink(&other, directory.path().join("state.yaml.tmp")).unwrap();
        paths.write_atomic(&target, "secret").unwrap();
        assert_eq!(std::fs::read_to_string(other).unwrap(), "keep");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "secret");
        assert_eq!(
            std::fs::metadata(target).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
