//! Local resource checks before a core can change routing or DNS.

use std::net::{SocketAddr, TcpListener, UdpSocket};

use super::{Config, Error, Result, Service};

impl Service {
    /// Refuse occupied listeners and competing Mihomo TUN routing.
    /// Existing listeners of this managed process are retained during reload.
    ///
    /// # Errors
    /// Returns a configuration error for a detected local resource conflict.
    pub fn validate_environment(&self, config: &Config) -> Result<()> {
        if config.tun_enabled() && crate::mihomo::resolver::available() {
            crate::mihomo::resolver::Target::from_config(config)?;
        }
        let current = if self.core_status().is_running() {
            self.paths
                .read(&self.paths.runtime_config())
                .ok()
                .and_then(|text| Config::from_yaml(&text).ok())
        } else {
            None
        };
        for key in ["external-controller", "external-controller-tls"] {
            if let Some(address) = config.get_str(key).filter(|address| !address.is_empty()) {
                if current
                    .as_ref()
                    .and_then(|config| config.get_str(key))
                    .as_deref()
                    == Some(address.as_str())
                {
                    continue;
                }
                check_listener(&address, false)?;
            }
        }
        if let Some(socket) = config
            .get_str("external-controller-unix")
            .filter(|path| !path.is_empty())
        {
            let owned = current
                .as_ref()
                .and_then(|old| old.get_str("external-controller-unix"))
                .as_deref()
                == Some(socket.as_str());
            if !owned && std::path::Path::new(&socket).exists() {
                return Err(Error::invalid(
                    "external-controller-unix",
                    "socket path already exists and is not owned by this managed process; choose a private socket path",
                ));
            }
        }
        for key in [
            "port",
            "socks-port",
            "mixed-port",
            "redir-port",
            "tproxy-port",
        ] {
            let Some(port) = config
                .get(key)
                .and_then(serde_json::Value::as_u64)
                .filter(|port| *port > 0)
            else {
                continue;
            };
            let host = if config.get("allow-lan").and_then(serde_json::Value::as_bool) == Some(true)
            {
                config
                    .get_str("bind-address")
                    .filter(|value| value != "*")
                    .unwrap_or_else(|| "0.0.0.0".to_owned())
            } else {
                "127.0.0.1".to_owned()
            };
            let unchanged = current.as_ref().is_some_and(|old| {
                old.get(key) == config.get(key)
                    && old.get("allow-lan") == config.get("allow-lan")
                    && old.get("bind-address") == config.get("bind-address")
            });
            if !unchanged {
                let address = if host.contains(':') {
                    format!("[{}]:{port}", host.trim_matches(['[', ']']))
                } else {
                    format!("{host}:{port}")
                };
                check_listener(&address, false)?;
            }
        }
        if config
            .get("dns")
            .and_then(|dns| dns.get("enable"))
            .and_then(serde_json::Value::as_bool)
            == Some(true)
            && let Some(address) = config
                .get("dns")
                .and_then(|dns| dns.get("listen"))
                .and_then(serde_json::Value::as_str)
        {
            let unchanged = current.as_ref().is_some_and(|old| {
                old.get("dns")
                    .and_then(|dns| dns.get("enable"))
                    .and_then(serde_json::Value::as_bool)
                    == Some(true)
                    && old
                        .get("dns")
                        .and_then(|dns| dns.get("listen"))
                        .and_then(serde_json::Value::as_str)
                        == Some(address)
            });
            if !unchanged {
                check_listener(address, false)?;
                check_listener(address, true)?;
            }
        }
        #[cfg(target_os = "linux")]
        if config.tun_enabled() {
            self.check_foreign_tun()?;
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn check_foreign_tun(&self) -> Result<()> {
        let own_pid = match self.core_status() {
            super::CoreStatus::Running { pid, .. } => Some(pid),
            _ => None,
        };
        let entries = std::fs::read_dir("/proc").map_err(|error| Error::io("/proc", error))?;
        for entry in entries.flatten() {
            let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
                continue;
            };
            if own_pid == Some(pid) {
                continue;
            }
            let directory = entry.path();
            let Some(identity) = live_process_identity(&directory) else {
                continue;
            };
            let name = std::fs::read_to_string(directory.join("comm")).unwrap_or_default();
            if !name.contains("mihomo") && !["clash", "clash-meta"].contains(&name.trim()) {
                continue;
            }
            let bytes = std::fs::read(directory.join("cmdline")).unwrap_or_default();
            let args: Vec<_> = bytes.split(|byte| *byte == 0).collect();
            // Parser/version subprocesses never install TUN routes even when
            // their input document enables TUN.
            if args
                .iter()
                .any(|arg| matches!(*arg, b"-t" | b"-v" | b"--version" | b"--test"))
            {
                continue;
            }
            let foreign = args
                .windows(2)
                .find(|pair| pair[0] == b"-f")
                .and_then(|pair| std::str::from_utf8(pair[1]).ok())
                .and_then(|path| std::fs::read_to_string(path).ok())
                .and_then(|text| Config::from_yaml(&text).ok());
            let conflict = foreign.as_ref().map_or_else(
                // A privileged foreign process may hide its configuration.
                // An existing Mihomo interface is then a reason to refuse,
                // rather than gamble with the system's default DNS/routes.
                || {
                    ["Mihomo", "cvt-mihomo"].iter().any(|name| {
                        std::path::Path::new(&format!("/sys/class/net/{name}/tun_flags")).exists()
                    })
                },
                Config::tun_enabled,
            );
            // The process may exit while its argv/config is being inspected.
            if conflict && live_process_identity(&directory) == Some(identity) {
                return Err(Error::invalid(
                    "tun",
                    format!(
                        "another Mihomo core (pid {pid}) has active or uninspectable TUN routing; disable its TUN or stop it before enabling TUN here"
                    ),
                ));
            }
        }
        Ok(())
    }
}

fn check_listener(address: &str, udp: bool) -> Result<()> {
    let normalized = if address.starts_with(':') {
        format!("0.0.0.0{address}")
    } else {
        address.to_owned()
    };
    let Ok(socket) = normalized.parse::<SocketAddr>() else {
        return Ok(());
    };
    if socket.port() == 0 {
        return Ok(());
    }
    // /proc catches a privileged-port conflict even when bind would fail
    // first with EACCES in this unprivileged client process.
    #[cfg(target_os = "linux")]
    if crate::mihomo::listeners::inspect(socket)
        .iter()
        .any(|owner| owner.protocol == if udp { "udp" } else { "tcp" })
    {
        return Err(conflict(address));
    }
    let result = if udp {
        UdpSocket::bind(socket).map(drop)
    } else {
        TcpListener::bind(socket).map(drop)
    };
    match result {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => Err(conflict(address)),
        Err(error) => Err(Error::invalid(
            "listener",
            format!("cannot bind {address}: {error}"),
        )),
    }
}

fn conflict(address: &str) -> Error {
    use crate::mihomo::listeners::{OwnerKind, inspect, parse_address};
    let owners = parse_address(address).map(inspect).unwrap_or_default();
    let details = owners
        .iter()
        .map(|owner| {
            let category = match owner.kind {
                OwnerKind::SystemDns => "system DNS service",
                OwnerKind::ProxyCore => "another proxy core",
                OwnerKind::Unknown => "unknown or inaccessible process",
            };
            format!(
                "{} {}: {category}, process {}, pid {}{}",
                owner.protocol,
                owner.address,
                owner.process.as_deref().unwrap_or("unknown"),
                owner
                    .pid
                    .map_or_else(|| "unknown".to_owned(), |pid| pid.to_string()),
                if owner.inferred {
                    " (inferred stub identity)"
                } else {
                    ""
                }
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    Error::invalid(
        "listener",
        format!(
            "{address} is already in use ({details}); keep the existing service and choose a separate local listener (for DNS, set core.dns_listen, e.g. 127.0.0.1:53 or 127.0.0.1:1053)"
        ),
    )
}

/// A live Linux process birth identity; zombies have released TUN descriptors.
#[cfg(target_os = "linux")]
fn live_process_identity(directory: &std::path::Path) -> Option<u64> {
    let stat = std::fs::read_to_string(directory.join("stat")).ok()?;
    let tail = stat.rsplit_once(')')?.1;
    if matches!(tail.split_whitespace().next(), Some("Z" | "X" | "x")) {
        return None;
    }
    tail.split_whitespace().nth(19)?.parse().ok()
}
