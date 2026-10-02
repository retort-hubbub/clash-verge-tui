//! DNS routing leases belong to a core birth identity and an interface index.
use super::{Supervisor, process_start_ticks};
use crate::mihomo::resolver::{self, DEVICE, Target};
use crate::model::config::Config;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Debug, Serialize, Deserialize)]
struct Lease {
    pid: u32,
    ticks: u64,
    index: u32,
    server: String,
}

impl Supervisor {
    fn dns_lease_path(&self) -> PathBuf {
        self.paths.core_dir().join("resolver.yaml")
    }

    fn dns_lease(&self) -> Result<Option<Lease>> {
        let path = self.dns_lease_path();
        match self.paths.read(&path) {
            Ok(text) => serde_norway::from_str(&text)
                .map(Some)
                .map_err(|e| Error::parse("DNS lease", path, e)),
            Err(Error::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    fn remove_dns_lease(&self) -> Result<()> {
        let path = self.dns_lease_path();
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Error::io(path, e)),
        }
    }

    /// Apply DNS routing only after the managed core and its TUN are ready.
    ///
    /// # Errors
    /// A failed handoff is a startup/reload failure, not silent DNS leakage.
    pub fn synchronize_dns(&self, config: &Config) -> Result<()> {
        if !cfg!(target_os = "linux") {
            return Ok(());
        }
        let Some(target) = (if config.tun_enabled() && resolver::available() {
            Target::from_config(config)?
        } else {
            None
        }) else {
            return self.release_dns();
        };
        let record = self
            .read_pid_record()
            .ok_or_else(|| Error::invalid("system DNS", "managed process identity is missing"))?;
        let deadline = Instant::now() + Duration::from_secs(10);
        let index = loop {
            self.check_health()?;
            if let Some(index) = interface_index() {
                break index;
            }
            if Instant::now() >= deadline {
                return Err(Error::invalid(
                    "system DNS",
                    "the managed TUN interface did not appear",
                ));
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        if !self.owns_process(&record)
            || process_start_ticks(record.pid) != Some(record.start_ticks)
        {
            return Err(Error::invalid(
                "system DNS",
                "core identity changed before DNS handoff",
            ));
        }
        let lease = Lease {
            pid: record.pid,
            ticks: record.start_ticks,
            index,
            server: target.server.to_string(),
        };
        if let Some(previous) = self.dns_lease()?
            && previous.pid == lease.pid
            && previous.ticks == lease.ticks
            && previous.index == lease.index
            && previous.server == lease.server
            && verify_dns(&index.to_string(), &target).is_ok()
        {
            return Ok(());
        }
        // Persist before mutating resolved so partial failures remain recoverable.
        self.paths.write_atomic(
            &self.dns_lease_path(),
            &serde_norway::to_string(&lease).map_err(|e| Error::serialize("DNS lease", e))?,
        )?;
        let link = index.to_string();
        let result = (|| {
            resolver::resolvectl(&["dns", &link, &lease.server])?;
            resolver::resolvectl(&["domain", &link, "~."])?;
            resolver::resolvectl(&["default-route", &link, "yes"])?;
            // Old poisoned answers must not outlive the DNS routing change.
            resolver::resolvectl(&["flush-caches"])?;
            verify_dns(&link, &target)
        })();
        if let Err(error) = result {
            return match self.release_dns() {
                Ok(()) => Err(error),
                Err(recovery) => Err(Error::invalid(
                    "system DNS",
                    format!("{error}; reverting partial DNS handoff failed: {recovery}"),
                )),
            };
        }
        Ok(())
    }

    /// Revert only the recorded Link. A deleted/recreated link is not adopted.
    ///
    /// # Errors
    /// Keep the lease if a live link could not be reset, allowing a later retry.
    pub fn release_dns(&self) -> Result<()> {
        if !cfg!(target_os = "linux") {
            return Ok(());
        }
        let Some(lease) = self.dns_lease()? else {
            return Ok(());
        };
        if interface_index() == Some(lease.index) {
            // No raw network-device name from a subscription reaches this command.
            resolver::resolvectl(&["revert", &lease.index.to_string()])?;
            resolver::resolvectl(&["flush-caches"])?;
        }
        self.remove_dns_lease()
    }

    pub(super) fn discard_vanished_dns_lease(&self) -> Result<bool> {
        if let Some(lease) = self.dns_lease()?
            && interface_index() == Some(lease.index)
        {
            return Ok(false);
        }
        self.remove_dns_lease()?;
        Ok(true)
    }
}

fn interface_index() -> Option<u32> {
    let path = format!("/sys/class/net/{DEVICE}");
    if !std::path::Path::new(&format!("{path}/tun_flags")).exists() {
        return None;
    }
    std::fs::read_to_string(format!("{path}/ifindex"))
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn verify_dns(link: &str, target: &Target) -> Result<()> {
    let dns = resolver::resolvectl(&["dns", link])?;
    let address = target.server.to_string();
    let plain = target.server.ip().to_string();
    if !dns
        .split_whitespace()
        .any(|token| token == address || (target.server.port() == 53 && token == plain))
    {
        return Err(Error::invalid(
            "system DNS",
            "resolved did not retain the local Mihomo DNS endpoint",
        ));
    }
    let domains = resolver::resolvectl(&["domain", link])?;
    let default = resolver::resolvectl(&["default-route", link])?;
    if !domains.split_whitespace().any(|domain| domain == "~.")
        || default.split_whitespace().last() != Some("yes")
    {
        return Err(Error::invalid(
            "system DNS",
            "resolved did not retain the TUN default DNS route",
        ));
    }
    Ok(())
}
