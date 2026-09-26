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

/// Locates, starts and stops the mihomo core.
#[derive(Debug, Clone)]
pub struct Supervisor {
    paths: AppPaths,
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

    /// Find the core binary.
    ///
    /// Order: `$CVT_CORE`, then `<home>/core/mihomo`, then `mihomo` on `PATH`.
    /// An explicit setting always wins, so a user can point at a build they
    /// compiled themselves.
    #[must_use]
    pub fn locate(&self, explicit: Option<&Path>) -> Option<PathBuf> {
        if let Some(p) = explicit.filter(|p| p.is_file()) {
            return Some(p.to_path_buf());
        }
        if let Some(from_env) = std::env::var_os(CORE_ENV) {
            let candidate = PathBuf::from(from_env);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        let bundled = self.paths.core_dir().join(exe_name());
        if bundled.is_file() {
            return Some(bundled);
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
