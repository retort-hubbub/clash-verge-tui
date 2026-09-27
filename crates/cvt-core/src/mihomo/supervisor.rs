//! Running the mihomo process.
//!
//! The core is an ordinary child process that this application starts, stops
//! and reloads. Two details make it worth a dedicated module rather than a few
//! `Command` calls:
//!
//! * **It must be validated before it is launched.** `mihomo -t` parses a
//!   configuration and exits without touching the network. Running that first
//!   turns a crash loop into one clear error message.
//! * **It must not be killed twice.** A terminal can be closed, a `SIGHUP`
//!   delivered, or two instances started against the same home. A pid file
//!   that records when the process started, and a signal sequence of `SIGTERM`
//!   and only then `SIGKILL`, keeps a stale pid from terminating an unrelated
//!   process that happens to have reused the number.
//!
//! Everything here is synchronous and short-lived on purpose: starting a
//! process is not a hot path, and making it blocking keeps the call sites in
//! the UI simple.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::paths::AppPaths;

/// Environment variable naming an explicit core binary.
pub const CORE_ENV: &str = "CVT_CORE";

/// How long `SIGTERM` is given before `SIGKILL` is sent.
pub const GRACEFUL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// What the supervisor knows about the core process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CoreStatus {
    /// No binary could be found.
    NotInstalled,
    /// A binary exists but nothing is running.
    Stopped,
    /// A process is running.
    Running {
        /// Process id.
        pid: u32,
        /// Unix time at which it was started.
        since: i64,
    },
    /// A pid file exists but no process answers to it.
    StalePid {
        /// The pid that was recorded.
        pid: u32,
    },
}

impl CoreStatus {
    /// `true` when the core is running.
    #[must_use]
    pub fn is_running(&self) -> bool {
        matches!(self, Self::Running { .. })
    }

    /// Short label for the status bar.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::NotInstalled => "no core binary".to_owned(),
            Self::Stopped => "stopped".to_owned(),
            Self::Running { pid, .. } => format!("running (pid {pid})"),
            Self::StalePid { pid } => format!("stale pid {pid}"),
        }
    }
}

/// The record written to the pid file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct PidRecord {
    pid: u32,
    /// Start time in unix seconds, used to detect pid reuse.
    since: i64,
    /// Process start time from `/proc/<pid>/stat` where available. Two
    /// processes with the same pid never share this.
    #[serde(default)]
    start_ticks: u64,
}

/// Rotating and pruning a log file, for whichever log is being rotated.
///
/// Kept apart from the supervisor because it is about files rather than about
/// the core: the same two operations apply to the application's own log, and
/// nothing here needs to know which one it is working on.
mod rotation {
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime};

    use crate::error::{Error, Result};

    /// The most files `keep` will ever be taken at face value.
    ///
    /// `Settings::validate` refuses more, but that only runs on the way *to*
    /// disk: a `cvt.yaml` somebody edited, or one written by an older version,
    /// is loaded without it. `rotate` probes one path per step, so a `keep` of
    /// a hundred million is minutes of syscalls on every start, twice.
    const KEEP_LIMIT: usize = 64;

    /// Where the `n`th rotated copy of `log` lives, `n` starting at 1.
    fn rotated(log: &Path, n: usize) -> PathBuf {
        let mut name = log.file_name().unwrap_or_default().to_os_string();
        name.push(format!(".{n}"));
        log.with_file_name(name)
    }

    pub(super) fn rotate(log: &Path, max_bytes: u64, keep: usize) -> Result<Option<PathBuf>> {
        let keep = keep.min(KEEP_LIMIT);
        if max_bytes == 0 || keep == 0 {
            return Ok(None);
        }
        let Ok(metadata) = std::fs::symlink_metadata(log) else {
            return Ok(None);
        };
        // A directory is not a log. A *symlink* is, and is handled below: the
        // size check follows it, so `rename(2)` would move the link and leave
        // the bytes it points at exactly where they were — the rotated "copy"
        // would point at a file nothing writes any more, and the user's log
        // destination would stop receiving output with nothing said about it.
        if metadata.is_dir() {
            return Err(Error::invalid(
                "log",
                format!("{} is a directory, so it cannot be rotated", log.display()),
            ));
        }
        let is_link = metadata.file_type().is_symlink();
        let target = if is_link {
            std::fs::canonicalize(log).unwrap_or_else(|_| log.to_path_buf())
        } else {
            log.to_path_buf()
        };
        let Ok(size) = std::fs::metadata(&target).map(|m| m.len()) else {
            return Ok(None);
        };
        if size < max_bytes {
            return Ok(None);
        }
        // Oldest first: dropping it makes room for the shift, and doing it
        // before the renames means a full directory never blocks them.
        let _ = std::fs::remove_file(rotated(log, keep));
        for n in (1..keep).rev() {
            let from = rotated(log, n);
            if from.is_file() {
                let to = rotated(log, n + 1);
                std::fs::rename(&from, &to).map_err(|e| Error::io(&to, e))?;
            }
        }
        let first = rotated(log, 1);
        if is_link {
            // Copied, then the target truncated. Copying keeps the user's
            // redirection intact — the link is the point of the setup — and
            // the truncation is what makes the rotation mean anything for a
            // file the writer may still hold open.
            std::fs::copy(&target, &first).map_err(|e| Error::io(&first, e))?;
            std::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&target)
                .map_err(|e| Error::io(&target, e))?;
        } else {
            std::fs::rename(log, &first).map_err(|e| Error::io(&first, e))?;
        }
        Ok(Some(first))
    }

    pub(super) fn prune(log: &Path, keep_days: u64) -> Result<usize> {
        if keep_days == 0 {
            return Ok(0);
        }
        let Some(directory) = log.parent() else {
            return Ok(0);
        };
        let Some(stem) = log.file_name().and_then(|name| name.to_str()) else {
            return Ok(0);
        };
        let cutoff = SystemTime::now()
            .checked_sub(Duration::from_secs(keep_days.saturating_mul(86_400)))
            .unwrap_or(SystemTime::UNIX_EPOCH);

        let mut removed = 0;
        let Ok(entries) = std::fs::read_dir(directory) else {
            return Ok(0);
        };
        // Every candidate is visited even when one of them cannot be removed.
        // It used to return at the first failure, which left the pass
        // *partially* done and made which copies survived depend on the order
        // `read_dir` happened to produce.
        let mut failure: Option<Error> = None;
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            // Only the files [`rotate`] writes: `<log>.<n>`, for an `n` of at
            // least one, spelled the way `to_string` spells it. A looser test
            // matched `core.log.0`, `.01`, `.+1` and `.007` — none of which
            // this module creates, and `core.log.0` is a spelling other
            // rotators do write.
            let Some(suffix) = name.strip_prefix(stem).and_then(|s| s.strip_prefix('.')) else {
                continue;
            };
            let Ok(index) = suffix.parse::<usize>() else {
                continue;
            };
            if index == 0 || suffix != index.to_string() {
                continue;
            }
            // A directory or a symlink that happens to carry the name is not
            // a rotated copy — this module writes regular files — so it is
            // skipped rather than reported. An unreadable *file* is a real
            // problem and is reported, after every other candidate has been
            // dealt with.
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }
            let Ok(modified) = metadata.modified() else {
                continue;
            };
            if modified >= cutoff {
                continue;
            }
            match std::fs::remove_file(entry.path()) {
                Ok(()) => removed += 1,
                Err(source) => {
                    failure.get_or_insert_with(|| Error::io(entry.path(), source));
                }
            }
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(removed),
        }
    }
}

/// Locates, starts and stops the mihomo core.
#[derive(Debug, Clone)]
pub struct Supervisor {
    paths: AppPaths,
}

/// The directories the core may read a configuration from, besides its own.
///
/// The document's own directory, so every `-f` this program passes is a path
/// the core will accept back through `PUT /configs?path=…`. A `-f` already
/// inside the core's home needs nothing, but saying so anyway is cheaper than
/// working out whether it does.
fn safe_path_for(config: &Path) -> std::ffi::OsString {
    let mut value = config
        .parent()
        .map_or_else(|| config.to_path_buf(), std::path::Path::to_path_buf);
    if let Some(existing) = std::env::var_os("SAFE_PATHS") {
        // Whatever the user already had stays; this only adds to it.
        value.push(":");
        value.push(existing);
    }
    value.into_os_string()
}

impl Supervisor {
    /// Bind a supervisor to an application home.
    #[must_use]
    pub fn new(paths: AppPaths) -> Self {
        Self { paths }
    }

    /// The pid file path.
    #[must_use]
    pub fn pid_file(&self) -> PathBuf {
        self.paths.core_dir().join("mihomo.pid")
    }

    /// Where the core's own output is captured.
    #[must_use]
    pub fn log_file(&self) -> PathBuf {
        self.paths.core_log()
    }

    /// Rotate a log file that has grown past `max_bytes`.
    ///
    /// Returns the file the old contents were moved to, or `None` when there
    /// was nothing to do.
    ///
    /// Called before the core is started, and that timing is the whole design:
    /// the child holds the file open for as long as it runs, so renaming it
    /// underneath would leave a live process writing into a file nothing will
    /// read again — the log would look frozen at the moment of rotation while
    /// silently growing somewhere else. At a start there is no child, and the
    /// file is about to be reopened.
    ///
    /// # Errors
    /// [`Error::Io`] when the file cannot be moved.
    pub fn rotate_log(&self, log: &Path, max_bytes: u64, keep: usize) -> Result<Option<PathBuf>> {
        rotation::rotate(log, max_bytes, keep)
    }

    /// Delete rotated logs older than `keep_days`.
    ///
    /// # Errors
    /// [`Error::Io`] when a file exists and cannot be removed.
    pub fn prune_logs(&self, log: &Path, keep_days: u64) -> Result<usize> {
        rotation::prune(log, keep_days)
    }

    /// Find the core binary.
    ///
    /// Order: `$CVT_CORE`, then `<home>/core/mihomo`, then `mihomo` on `PATH`.
    /// An explicit setting always wins, so a user can point at a build they
    /// compiled themselves.
    #[must_use]
    pub fn locate(&self, explicit: Option<&Path>) -> Option<PathBuf> {
        self.locate_with(explicit, true)
    }

    /// Find the core binary, respecting whether the user prefers the managed core.
    #[must_use]
    pub fn locate_with(&self, explicit: Option<&Path>, use_managed: bool) -> Option<PathBuf> {
        if let Some(p) = explicit.filter(|p| p.is_file()) {
            return Some(p.to_path_buf());
        }
        if let Some(from_env) = std::env::var_os(CORE_ENV) {
            let candidate = PathBuf::from(from_env);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        if use_managed {
            let bundled = self.paths.core_dir().join(exe_name());
            if bundled.is_file() {
                return Some(bundled);
            }
        }
        which(exe_name())
    }

    /// Report the running state, using the pid file as the only authority.
    #[must_use]
    pub fn status(&self) -> CoreStatus {
        let Some(record) = self.read_pid_record() else {
            return CoreStatus::Stopped;
        };
        if !process_alive(record.pid) || !pid_matches(record) {
            return CoreStatus::StalePid { pid: record.pid };
        }
        CoreStatus::Running {
            pid: record.pid,
            since: record.since,
        }
    }

    /// Ask the core to check a configuration without running it.
    ///
    /// This is `mihomo -t -d <dir> -f <file>`. Running it before every apply
    /// converts a crash-on-start into a readable message.
    ///
    /// # Errors
    /// [`Error::CoreUnavailable`] when no binary is found, and
    /// [`Error::ProcessFailed`] when validation reports a problem.
    pub fn validate_config(&self, binary: &Path, config: &Path) -> Result<()> {
        let output = Command::new(binary)
            .arg("-t")
            .arg("-d")
            .arg(self.paths.core_work_dir())
            .arg("-f")
            .arg(config)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| Error::ProcessFailed {
                program: binary.display().to_string(),
                status: "spawn failed".to_owned(),
                stderr: e.to_string(),
            })?;
        if output.status.success() {
            return Ok(());
        }
        Err(Error::ProcessFailed {
            program: format!("{} -t", binary.display()),
            status: output.status.to_string(),
            stderr: summarise(&String::from_utf8_lossy(&output.stderr)),
        })
    }

    /// Launch the core with a generated configuration.
    ///
    /// Stdout and stderr are appended to the core log so that a start-up
    /// failure is diagnosable after the fact, and stdin is closed so the
    /// process cannot block on a terminal that has gone away.
    ///
    /// # Errors
    /// [`Error::Unsupported`] when the core is already running, and
    /// [`Error::ProcessFailed`] when it cannot be spawned.
    pub fn start(&self, binary: &Path, config: &Path) -> Result<u32> {
        if let CoreStatus::Running { pid, .. } = self.status() {
            return Err(Error::Unsupported(format!(
                "the core is already running as pid {pid}; stop it first"
            )));
        }
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.log_file())
            .map_err(|e| Error::io(self.log_file(), e))?;
        let log_err = log.try_clone().map_err(|e| Error::io(self.log_file(), e))?;

        let child = Command::new(binary)
            .arg("-d")
            .arg(self.paths.core_work_dir())
            .arg("-f")
            .arg(config)
            // The generated configuration lives outside the core's own home,
            // and mihomo refuses any `path` that is not under it:
            //
            //   400 path is not subpath of home directory or SAFE_PATHS: …
            //       allowed paths: […/core/work]
            //
            // So `PUT /configs?path=…` — the whole hot-reload path — could
            // never succeed: `--mode hot` always failed, and `--mode auto`, the
            // default, always fell back to a *restart*, which is the one thing
            // the reload design exists to avoid because a restart drops every
            // live connection. `SAFE_PATHS` is how the core is told which other
            // directories it may read a configuration from.
            .env("SAFE_PATHS", safe_path_for(config))
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err))
            .spawn()
            .map_err(|e| Error::ProcessFailed {
                program: binary.display().to_string(),
                status: "spawn failed".to_owned(),
                stderr: e.to_string(),
            })?;

        let pid = child.id();
        let record = PidRecord {
            pid,
            since: chrono::Utc::now().timestamp(),
            start_ticks: process_start_ticks(pid).unwrap_or(0),
        };
        let yaml = serde_norway::to_string(&record).map_err(|e| Error::serialize("pid file", e))?;
        self.paths.write_atomic(&self.pid_file(), &yaml)?;
        Ok(pid)
    }

    /// Stop the core, escalating from `SIGTERM` to `SIGKILL`.
    ///
    /// Returns `true` when a process was actually stopped.
    ///
    /// # Errors
    /// [`Error::ProcessFailed`] when the signal could not be delivered to a
    /// process that is confirmed to be ours.
    pub fn stop(&self) -> Result<bool> {
        let status = self.status();
        let pid = match status {
            CoreStatus::Running { pid, .. } => pid,
            CoreStatus::StalePid { .. } => {
                // Nothing to signal; just clear the record.
                let _ = std::fs::remove_file(self.pid_file());
                return Ok(false);
            }
            _ => return Ok(false),
        };

        terminate(pid, false)?;
        let deadline = std::time::Instant::now() + GRACEFUL_TIMEOUT;
        while std::time::Instant::now() < deadline {
            if !process_alive(pid) {
                let _ = std::fs::remove_file(self.pid_file());
                return Ok(true);
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        terminate(pid, true)?;
        // Give the kernel a moment to reap before reporting.
        std::thread::sleep(std::time::Duration::from_millis(100));
        let _ = std::fs::remove_file(self.pid_file());
        Ok(true)
    }

    /// Stop and start again.
    ///
    /// # Errors
    /// Propagates stop and start failures.
    pub fn restart(&self, binary: &Path, config: &Path) -> Result<u32> {
        self.stop()?;
        self.start(binary, config)
    }

    /// Read the core's version by running `mihomo -v`.
    ///
    /// # Errors
    /// [`Error::ProcessFailed`] when the binary cannot be run.
    pub fn version(&self, binary: &Path) -> Result<String> {
        let output = Command::new(binary)
            .arg("-v")
            .stdin(Stdio::null())
            .output()
            .map_err(|e| Error::ProcessFailed {
                program: binary.display().to_string(),
                status: "spawn failed".to_owned(),
                stderr: e.to_string(),
            })?;
        let text = String::from_utf8_lossy(&output.stdout);
        // `mihomo -v` prints "Mihomo Meta v1.19.31 ..." on one line.
        Ok(text.lines().next().unwrap_or_default().trim().to_owned())
    }

    /// Read the pid record, tolerating a truncated or foreign file.
    fn read_pid_record(&self) -> Option<PidRecord> {
        let text = std::fs::read_to_string(self.pid_file()).ok()?;
        serde_norway::from_str(&text).ok()
    }
}

/// Guard against pid reuse: a recycled pid must not be mistaken for ours.
fn pid_matches(record: PidRecord) -> bool {
    if record.start_ticks == 0 {
        return true; // the field was unavailable, so do not second-guess
    }
    process_start_ticks(record.pid) == Some(record.start_ticks)
}

fn exe_name() -> &'static str {
    if cfg!(windows) {
        "mihomo.exe"
    } else {
        "mihomo"
    }
}

/// Search `PATH` for a program.
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// Convert a pid into a typed handle, rejecting values that are not a single
/// process. Notably `0`, which `kill(2)` would interpret as "every process in
/// my group" — the one value that must never reach a signal call.
#[cfg(unix)]
fn pid_handle(pid: u32) -> Option<rustix::process::Pid> {
    i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
}

/// Whether a process exists, without assuming we may signal it.
fn process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let Some(handle) = pid_handle(pid) else {
            return false;
        };
        match rustix::process::test_kill_process(handle) {
            Ok(()) => true,
            // EPERM means it exists but belongs to another user.
            Err(e) => e == rustix::io::Errno::PERM,
        }
    }
    #[cfg(not(unix))]
    {
        // No portable check without a handle; the pid file is the authority.
        let _ = pid;
        true
    }
}

/// Deliver `SIGTERM`, or `SIGKILL` when `force` is set.
fn terminate(pid: u32, force: bool) -> Result<()> {
    #[cfg(unix)]
    {
        let Some(handle) = pid_handle(pid) else {
            // Nothing we could have started; treat as already gone.
            return Ok(());
        };
        let signal = if force {
            rustix::process::Signal::KILL
        } else {
            rustix::process::Signal::TERM
        };
        match rustix::process::kill_process(handle, signal) {
            // ESRCH means it exited between our check and the signal, which is
            // the outcome we wanted.
            Ok(()) | Err(rustix::io::Errno::SRCH) => Ok(()),
            Err(e) => Err(Error::ProcessFailed {
                program: format!("kill({pid})"),
                status: if force { "SIGKILL" } else { "SIGTERM" }.to_owned(),
                stderr: e.to_string(),
            }),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (pid, force);
        Err(Error::Unsupported(
            "stopping the core is only implemented on unix".to_owned(),
        ))
    }
}

/// Field 22 of `/proc/<pid>/stat`: process start time in clock ticks.
///
/// Used to tell a live process from a recycled pid. Returns `None` where the
/// information is unavailable, in which case the caller trusts the pid.
fn process_start_ticks(pid: u32) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // The comm field is parenthesised and may contain spaces, so split
        // after the final ')' rather than on whitespace.
        let after = stat.rsplit_once(')')?.1;
        after.split_whitespace().nth(19)?.parse().ok()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        None
    }
}

/// Keep the informative tail of a subprocess error.
fn summarise(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= 800 {
        return trimmed.to_owned();
    }
    let tail: String = trimmed
        .chars()
        .rev()
        .take(800)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("...{tail}")
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn supervisor() -> (TempDir, Supervisor) {
        let dir = TempDir::new().unwrap();
        let paths = AppPaths::new(dir.path());
        paths.ensure_dirs().unwrap();
        let sup = Supervisor::new(paths);
        (dir, sup)
    }

    #[test]
    fn a_log_below_the_limit_is_left_alone() {
        let (_d, sup) = supervisor();
        let log = sup.log_file();
        std::fs::write(&log, "a few lines\n").unwrap();

        assert!(sup.rotate_log(&log, 1024, 4).unwrap().is_none());
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "a few lines\n");
        assert!(!sup.paths.logs_dir().join("core.log.1").exists());
    }

    #[test]
    fn a_log_above_the_limit_is_shifted_and_the_oldest_dropped() {
        let (_d, sup) = supervisor();
        let log = sup.log_file();
        for n in 1..=3 {
            std::fs::write(
                sup.paths.logs_dir().join(format!("core.log.{n}")),
                format!("old {n}\n"),
            )
            .unwrap();
        }
        std::fs::write(&log, "x".repeat(2048)).unwrap();

        let moved = sup.rotate_log(&log, 1024, 3).unwrap().unwrap();
        assert_eq!(moved, sup.paths.logs_dir().join("core.log.1"));
        // The live file is emptied of its old contents, and each copy has moved
        // along by one — with `keep` of 3, the third is gone rather than `.4`.
        assert_eq!(std::fs::read_to_string(&moved).unwrap().len(), 2048);
        assert_eq!(
            std::fs::read_to_string(sup.paths.logs_dir().join("core.log.2")).unwrap(),
            "old 1\n"
        );
        assert_eq!(
            std::fs::read_to_string(sup.paths.logs_dir().join("core.log.3")).unwrap(),
            "old 2\n"
        );
        assert!(!sup.paths.logs_dir().join("core.log.4").exists());
        // The live file is *moved*, not truncated: its contents are the point
        // of rotating, and the core recreates it when it is started — which is
        // the very next thing that happens, since this runs on the way in.
        assert!(!log.exists());
    }

    #[test]
    fn rotation_can_be_turned_off() {
        let (_d, sup) = supervisor();
        let log = sup.log_file();
        std::fs::write(&log, "x".repeat(4096)).unwrap();
        assert!(sup.rotate_log(&log, 0, 4).unwrap().is_none());
        assert_eq!(std::fs::read_to_string(&log).unwrap().len(), 4096);
    }

    #[test]
    fn pruning_takes_rotated_copies_and_nothing_else() {
        let (_d, sup) = supervisor();
        let dir = sup.paths.logs_dir();
        let log = sup.log_file();
        std::fs::write(&log, "live\n").unwrap();
        // A file that merely starts with the log's name is not a rotated copy.
        std::fs::write(dir.join("core.log.backup"), "keep me\n").unwrap();
        std::fs::write(dir.join("app.log.1"), "another log\n").unwrap();

        for n in 1..=3 {
            std::fs::write(dir.join(format!("core.log.{n}")), "old\n").unwrap();
        }
        // Everything just written is newer than a 14-day cutoff.
        assert_eq!(sup.prune_logs(&log, 14).unwrap(), 0);
        assert!(dir.join("core.log.1").exists());
        assert!(dir.join("core.log.backup").exists());
        assert!(dir.join("app.log.1").exists());
        // And a zero-day cutoff prunes nothing rather than everything.
        assert_eq!(sup.prune_logs(&log, 0).unwrap(), 0);
        assert!(dir.join("core.log.1").exists());
    }

    #[test]
    fn a_fresh_home_reports_not_running() {
        let (_d, sup) = supervisor();
        assert_eq!(sup.status(), CoreStatus::Stopped);
        assert!(!sup.status().is_running());
        assert_eq!(sup.status().label(), "stopped");
    }

    #[test]
    fn locates_a_core_from_the_environment_and_from_the_home_directory() {
        let (_d, sup) = supervisor();
        // A file that is certainly a file.
        let fake = sup.paths.core_dir().join(exe_name());
        std::fs::write(&fake, b"#!/bin/sh\nexit 0\n").unwrap();
        assert_eq!(sup.locate(None), Some(fake.clone()));
        // An explicit path wins over everything.
        let other = sup.paths.core_dir().join("my-mihomo");
        std::fs::write(&other, b"x").unwrap();
        assert_eq!(sup.locate(Some(&other)), Some(other));
        // A non-existent explicit path falls through rather than failing.
        assert_eq!(sup.locate(Some(Path::new("/nope/mihomo"))), Some(fake));
    }

    #[test]
    fn pid_zero_is_never_treated_as_a_live_process() {
        // `kill(0, sig)` signals the caller's entire process group, so a pid
        // file containing 0 must never result in a signal being sent.
        assert!(!process_alive(0), "pid 0 must not be considered alive");
        assert!(sup_terminate_zero().is_ok(), "and must not be signalled");
    }

    fn sup_terminate_zero() -> Result<()> {
        let (_d, sup) = supervisor();
        sup.paths
            .write_atomic(&sup.pid_file(), "pid: 0\nsince: 1\nstart_ticks: 1\n")
            .unwrap();
        assert_eq!(sup.status(), CoreStatus::StalePid { pid: 0 });
        terminate(0, false)
    }

    #[test]
    fn a_recycled_pid_does_not_match_the_recorded_one() {
        let mismatched = PidRecord {
            pid: std::process::id(),
            since: 1,
            start_ticks: u64::MAX,
        };
        assert!(
            !pid_matches(mismatched),
            "a recycled or foreign pid must not match"
        );
    }

    #[test]
    fn a_matching_pid_record_is_accepted() {
        let pid = std::process::id();
        let ticks = process_start_ticks(pid).unwrap_or(0);
        let record = PidRecord {
            pid,
            since: 1,
            start_ticks: ticks,
        };
        assert!(pid_matches(record));
        assert!(process_alive(pid), "the test process is obviously alive");
    }

    #[test]
    fn a_truncated_pid_file_does_not_panic() {
        let (_d, sup) = supervisor();
        sup.paths
            .write_atomic(&sup.pid_file(), "pid: [unclosed")
            .unwrap();
        assert_eq!(sup.status(), CoreStatus::Stopped);
        sup.paths.write_atomic(&sup.pid_file(), "").unwrap();
        assert_eq!(sup.status(), CoreStatus::Stopped);
    }

    #[test]
    fn stopping_when_nothing_runs_is_a_no_op() {
        let (_d, sup) = supervisor();
        assert!(!sup.stop().unwrap());
    }

    #[test]
    fn stopping_with_a_stale_pid_clears_the_file_without_signalling() {
        let (_d, sup) = supervisor();
        sup.paths
            .write_atomic(&sup.pid_file(), "pid: 0\nsince: 1\nstart_ticks: 1\n")
            .unwrap();
        assert!(!sup.stop().unwrap());
        assert!(!sup.pid_file().exists(), "the stale record is removed");
    }

    #[test]
    fn process_start_ticks_is_stable_for_this_process() {
        if cfg!(target_os = "linux") {
            let pid = std::process::id();
            let a = process_start_ticks(pid);
            let b = process_start_ticks(pid);
            assert!(a.is_some(), "/proc should expose the start time on Linux");
            assert_eq!(a, b);
        }
    }

    #[test]
    fn process_liveness_is_reported_for_this_process_and_an_impossible_one() {
        assert!(process_alive(std::process::id()));
        // Pid 2^31-1 is above the default pid_max on Linux and never allocatable.
        assert!(!process_alive(2_147_483_647));
    }

    #[test]
    fn validate_config_reports_a_missing_binary_clearly() {
        let (_d, sup) = supervisor();
        let err = sup
            .validate_config(Path::new("/nonexistent/mihomo"), Path::new("/tmp/x.yaml"))
            .unwrap_err();
        assert!(matches!(err, Error::ProcessFailed { .. }), "{err:?}");
        assert!(err.to_string().contains("mihomo"), "{err}");
    }

    #[test]
    fn validate_config_reports_the_core_output_on_failure() {
        let (_d, sup) = supervisor();
        // /bin/false exits non-zero and prints nothing.
        let bytes = if Path::new("/bin/false").exists() {
            "/bin/false"
        } else {
            "/usr/bin/false"
        };
        if !Path::new(bytes).exists() {
            return; // no usable stand-in on this machine
        }
        let err = sup
            .validate_config(Path::new(bytes), Path::new("/tmp/x.yaml"))
            .unwrap_err();
        match err {
            Error::ProcessFailed { program, .. } => assert!(program.contains("-t"), "{program}"),
            other => panic!("expected ProcessFailed, got {other:?}"),
        }
    }

    #[test]
    fn starting_when_already_running_is_refused() {
        let (_d, sup) = supervisor();
        // Pretend this very process is the core.
        let pid = std::process::id();
        let ticks = process_start_ticks(pid).unwrap_or(0);
        let record = PidRecord {
            pid,
            since: 1,
            start_ticks: ticks,
        };
        sup.paths
            .write_atomic(&sup.pid_file(), &serde_norway::to_string(&record).unwrap())
            .unwrap();
        assert!(sup.status().is_running());
        let err = sup
            .start(Path::new("/bin/true"), Path::new("/tmp/x.yaml"))
            .unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)), "{err:?}");
        assert!(err.to_string().contains("already running"), "{err}");
    }

    #[test]
    fn long_output_is_truncated_to_its_tail() {
        let long = "x".repeat(2000) + "TAIL";
        let s = summarise(&long);
        assert!(s.chars().count() <= 804, "{}", s.chars().count());
        assert!(s.ends_with("TAIL"));
        assert_eq!(summarise("  short  "), "short");
    }
}
