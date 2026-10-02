//! Local resource checks before a core can change routing or DNS.

#[cfg(target_os = "linux")]
use std::net::IpAddr;
use std::net::{SocketAddr, TcpListener, UdpSocket};

use super::{Config, Error, Result, Service};

impl Service {
    /// Refuse occupied listeners and competing Mihomo TUN routing.
    /// Existing listeners of this managed process are retained during reload.
    ///
    /// # Errors
    /// Returns a configuration error for a detected local resource conflict.
    pub fn validate_environment(&self, config: &Config) -> Result<()> {
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
            let name = std::fs::read_to_string(directory.join("comm")).unwrap_or_default();
            if !name.contains("mihomo") && !["clash", "clash-meta"].contains(&name.trim()) {
                continue;
            }
            let bytes = std::fs::read(directory.join("cmdline")).unwrap_or_default();
            let args: Vec<_> = bytes.split(|byte| *byte == 0).collect();
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
            if conflict {
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
    if occupied(socket, udp) {
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
    Error::invalid(
        "listener",
        format!(
            "{address} is already in use; stop the conflicting listener or choose another port (for DNS, e.g. 127.0.0.1:1053)"
        ),
    )
}

#[cfg(target_os = "linux")]
fn occupied(socket: SocketAddr, udp: bool) -> bool {
    let protocol = if udp { "udp" } else { "tcp" };
    [
        format!("/proc/net/{protocol}"),
        format!("/proc/net/{protocol}6"),
    ]
    .iter()
    .any(|path| {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .skip(1)
            .any(|line| {
                let fields: Vec<_> = line.split_whitespace().collect();
                if !udp && fields.get(3) != Some(&"0A") {
                    return false;
                }
                let Some((ip, port)) = fields.get(1).and_then(|value| value.split_once(':')) else {
                    return false;
                };
                if u16::from_str_radix(port, 16).ok() != Some(socket.port()) {
                    return false;
                }
                let address: Option<IpAddr> = if ip.len() == 8 {
                    u32::from_str_radix(ip, 16)
                        .ok()
                        .map(|value| std::net::Ipv4Addr::from(value.to_ne_bytes()).into())
                } else if ip.len() == 32 {
                    let mut bytes = [0; 16];
                    for (index, chunk) in ip.as_bytes().chunks(8).enumerate() {
                        let Some(value) = std::str::from_utf8(chunk)
                            .ok()
                            .and_then(|text| u32::from_str_radix(text, 16).ok())
                        else {
                            return false;
                        };
                        bytes[index * 4..index * 4 + 4].copy_from_slice(&value.to_ne_bytes());
                    }
                    Some(std::net::Ipv6Addr::from(bytes).into())
                } else {
                    None
                };
                address.is_some_and(|ip| {
                    ip == socket.ip() || ip.is_unspecified() || socket.ip().is_unspecified()
                })
            })
    })
}
