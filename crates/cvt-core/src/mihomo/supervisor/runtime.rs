//! Bounded parser checks, startup health and noninteractive resolver helpers.

use std::path::Path;
use std::process::{Command, Stdio};

use super::{CoreStatus, Supervisor, summarise};
use crate::error::{Error, Result};

impl Supervisor {
    /// Controller actually handed to the supervised process, independent of
    /// a selected profile or a candidate already written on disk.
    #[must_use]
    pub fn controller_endpoint(&self) -> Option<crate::mihomo::endpoint::Endpoint> {
        if !self.status().is_running() {
            return None;
        }
        self.read_pid_record()?.endpoint
    }

    /// Record the controller after a successful configuration handoff.
    ///
    /// # Errors
    /// Returns a configuration or PID file error.
    pub fn finish_reload(&self) -> Result<()> {
        let mut record = self
            .read_pid_record()
            .ok_or_else(|| Error::invalid("core", "missing process identity"))?;
        let config = crate::model::config::Config::from_yaml(
            &self.paths.read(&self.paths.runtime_config())?,
        )?;
        record.endpoint = crate::mihomo::endpoint::Endpoint::from_config(&config);
        let yaml = serde_norway::to_string(&record)
            .map_err(|error| Error::serialize("pid file", error))?;
        self.paths.write_atomic(&self.pid_file(), &yaml)
    }

    /// Start a fresh error window before hot reload, excluding earlier logs.
    ///
    /// # Errors
    /// Returns an error when process identity or the log cannot be read.
    pub fn begin_reload(&self) -> Result<()> {
        let mut record = self
            .read_pid_record()
            .ok_or_else(|| Error::invalid("core", "missing process identity"))?;
        if !matches!(self.status(), CoreStatus::Running { pid, .. } if pid == record.pid) {
            return Err(Error::invalid(
                "core",
                "process identity changed before reload",
            ));
        }
        record.log_offset = std::fs::metadata(self.log_file())
            .map_err(|error| Error::io(self.log_file(), error))?
            .len();
        let yaml = serde_norway::to_string(&record)
            .map_err(|error| Error::serialize("pid file", error))?;
        self.paths.write_atomic(&self.pid_file(), &yaml)
    }

    /// DNS and TUN listener errors are nonfatal to Mihomo's controller.
    /// Read only this launch's bounded log tail instead of trusting `/version`.
    ///
    /// # Errors
    /// Returns an error for lost process identity or known listener failures.
    pub fn check_health(&self) -> Result<()> {
        let record = self
            .read_pid_record()
            .ok_or_else(|| Error::invalid("core", "missing process identity"))?;
        if !matches!(self.status(), CoreStatus::Running { pid, .. } if pid == record.pid) {
            return Err(Error::invalid(
                "core",
                "managed core exited or process identity changed",
            ));
        }
        use std::io::{Read as _, Seek as _, SeekFrom};
        let mut file = std::fs::File::open(self.log_file())
            .map_err(|error| Error::io(self.log_file(), error))?;
        let end = file
            .metadata()
            .map_err(|error| Error::io(self.log_file(), error))?
            .len();
        file.seek(SeekFrom::Start(
            record.log_offset.max(end.saturating_sub(65_536)),
        ))
        .map_err(|error| Error::io(self.log_file(), error))?;
        let mut bytes = Vec::new();
        file.take(65_536)
            .read_to_end(&mut bytes)
            .map_err(|error| Error::io(self.log_file(), error))?;
        let text = String::from_utf8_lossy(&bytes);
        if let Some(line) = text.lines().find(|line| {
            line.contains("Start TUN listening error")
                || line.contains("proxy listening error")
                || line.contains("External controller listen error")
                || line.contains("Start DNS server(UDP) error")
                || line.contains("Start DNS server(TCP) error")
        }) {
            return Err(Error::invalid("core startup", line));
        }
        Ok(())
    }

    /// Keep a capability-based core from opening interactive system DNS
    /// authorization dialogs. DNS interception is configured in the TUN itself.
    pub(super) fn child_path(&self) -> Result<std::ffi::OsString> {
        let current = std::env::var_os("PATH").unwrap_or_default();
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::PermissionsExt as _;
            if let Some(resolvectl) = ["/usr/bin/resolvectl", "/bin/resolvectl"]
                .into_iter()
                .find(|path| Path::new(path).is_file())
            {
                let directory = self.paths.core_dir().join("helpers");
                std::fs::create_dir_all(&directory)
                    .map_err(|error| Error::io(&directory, error))?;
                let wrapper = directory.join("resolvectl");
                self.paths.write_atomic(
                    &wrapper,
                    &format!("#!/bin/sh\nexec {resolvectl} --no-ask-password \"$@\"\n"),
                )?;
                std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700))
                    .map_err(|error| Error::io(&wrapper, error))?;
                let mut paths = vec![directory];
                paths.extend(std::env::split_paths(&current));
                return std::env::join_paths(paths)
                    .map_err(|error| Error::Unsupported(error.to_string()));
            }
        }
        Ok(current)
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
        let output = tempfile::tempfile_in(self.paths.core_dir())
            .map_err(|error| Error::io(self.paths.core_dir(), error))?;
        let mut child = Command::new(binary)
            .args(["-t", "-d"])
            .arg(self.paths.core_work_dir())
            .arg("-f")
            .arg(config)
            .stdin(Stdio::null())
            .stdout(Stdio::from(
                output
                    .try_clone()
                    .map_err(|error| Error::io(config, error))?,
            ))
            .stderr(Stdio::from(
                output
                    .try_clone()
                    .map_err(|error| Error::io(config, error))?,
            ))
            .spawn()
            .map_err(|error| Error::ProcessFailed {
                program: binary.display().to_string(),
                status: "spawn failed".to_owned(),
                stderr: error.to_string(),
            })?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(25))
                }
                other => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(Error::ProcessFailed {
                        program: format!("{} -t", binary.display()),
                        status: "validation did not finish".to_owned(),
                        stderr: other.err().map_or_else(
                            || "configuration validation timed out after 30 seconds".to_owned(),
                            |error| error.to_string(),
                        ),
                    });
                }
            }
        };
        if status.success() {
            return Ok(());
        }
        use std::io::{Read as _, Seek as _, SeekFrom};
        let mut output = output;
        output
            .seek(SeekFrom::Start(0))
            .map_err(|error| Error::io(config, error))?;
        let mut bytes = Vec::new();
        output
            .take(65_536)
            .read_to_end(&mut bytes)
            .map_err(|error| Error::io(config, error))?;
        let text = String::from_utf8_lossy(&bytes);
        Err(Error::ProcessFailed {
            program: format!("{} -t", binary.display()),
            status: status.to_string(),
            stderr: summarise(&text),
        })
    }
}
