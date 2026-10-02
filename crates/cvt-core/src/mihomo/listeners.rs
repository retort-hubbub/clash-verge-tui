//! Read-only listener ownership inspection and local DNS conflict alternatives.
use std::net::{IpAddr, SocketAddr, TcpListener, UdpSocket};

use crate::model::config::Config;

/// Classification is separate from translated UI text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerKind {
    /// A recognized DNS service or a resolved stub inferred from its active service.
    SystemDns,
    /// Another proxy core with an inspectable socket owner.
    ProxyCore,
    /// An unknown process, or ownership unavailable without additional privileges.
    Unknown,
}

/// One conflicting socket and its observable owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenerOwner {
    /// Listener address, not the requested wildcard address.
    pub address: SocketAddr,
    /// TCP or UDP.
    pub protocol: String,
    /// Classification used by the UI.
    pub kind: OwnerKind,
    /// Process name when it is visible.
    pub process: Option<String>,
    /// Process identifier when it is visible.
    pub pid: Option<u32>,
    /// True when classification relies on the active resolved stub endpoint.
    pub inferred: bool,
}

/// A recoverable conflict; acceptance changes only the application's listener.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DnsConflict {
    /// Subscription/application listener that conflicts.
    pub requested: String,
    /// Observable conflicting listeners.
    pub owners: Vec<ListenerOwner>,
    /// A candidate local replacement. Availability is rechecked before launch.
    pub replacement: String,
}

/// Inspect a DNS listener and propose an alternative without changing the host.
#[must_use]
pub fn dns_conflict(config: &Config) -> Option<DnsConflict> {
    let dns = config.get("dns")?;
    if !dns.get("enable")?.as_bool()? {
        return None;
    }
    let requested = dns.get("listen")?.as_str()?;
    let socket = parse_address(requested)?;
    if socket.port() == 0 {
        return None;
    }
    let owners = inspect(socket);
    if owners.is_empty() && bindable(socket) {
        return None;
    }
    // Keep port 53 when the conflict only concerns another loopback address.
    let replacement = std::iter::once(socket.port())
        .chain(1053..=1063)
        .map(|port| SocketAddr::from(([127, 0, 0, 1], port)))
        .find(|candidate| {
            *candidate != socket && inspect(*candidate).is_empty() && bindable(*candidate)
        })?;
    Some(DnsConflict {
        requested: requested.to_owned(),
        owners,
        replacement: replacement.to_string(),
    })
}

/// Accept shorthand `:53`, used by Mihomo, as an IPv4 wildcard listener.
#[must_use]
pub fn parse_address(address: &str) -> Option<SocketAddr> {
    if address.starts_with(':') {
        format!("0.0.0.0{address}").parse().ok()
    } else {
        address.parse().ok()
    }
}

fn bindable(socket: SocketAddr) -> bool {
    let tcp = TcpListener::bind(socket);
    let udp = UdpSocket::bind(socket);
    [tcp.as_ref().map(|_| ()), udp.as_ref().map(|_| ())]
        .iter()
        .all(|result| {
            result.is_ok()
                || result
                    .as_ref()
                    .is_err_and(|error| error.kind() == std::io::ErrorKind::PermissionDenied)
        })
}

/// Linux proc tables are inspected without elevated privileges. Missing owner
/// information stays unknown; no process is killed or service stopped.
#[must_use]
pub fn inspect(socket: SocketAddr) -> Vec<ListenerOwner> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = socket;
        Vec::new()
    }
    #[cfg(target_os = "linux")]
    {
        let mut found = Vec::new();
        for protocol in ["tcp", "udp"] {
            for suffix in ["", "6"] {
                let table = std::fs::read_to_string(format!("/proc/net/{protocol}{suffix}"))
                    .unwrap_or_default();
                for line in table.lines().skip(1) {
                    let fields: Vec<_> = line.split_whitespace().collect();
                    if protocol == "tcp" && fields.get(3) != Some(&"0A") {
                        continue;
                    }
                    let Some(address) = fields.get(1).and_then(|address| proc_address(address))
                    else {
                        continue;
                    };
                    if address.port() != socket.port() || !overlaps(address.ip(), socket.ip()) {
                        continue;
                    }
                    let (pid, process) = fields
                        .get(9)
                        .and_then(|inode| owner(inode))
                        .map_or((None, None), |(pid, name)| (Some(pid), Some(name)));
                    let stub = matches!(address.ip(), IpAddr::V4(ip) if ip.octets() == [127,0,0,53] || ip.octets() == [127,0,0,54])
                        && address.port() == 53
                        && super::resolver::available();
                    let kind = match process.as_deref() {
                        Some(
                            "systemd-resolve" | "systemd-resolved" | "dnsmasq" | "unbound"
                            | "named",
                        ) => OwnerKind::SystemDns,
                        Some(name)
                            if name.contains("mihomo")
                                || matches!(
                                    name,
                                    "clash" | "clash-meta" | "sing-box" | "xray" | "v2ray"
                                ) =>
                        {
                            OwnerKind::ProxyCore
                        }
                        None if stub => OwnerKind::SystemDns,
                        _ => OwnerKind::Unknown,
                    };
                    found.push(ListenerOwner {
                        address,
                        protocol: protocol.to_owned(),
                        kind,
                        inferred: process.is_none() && stub,
                        process,
                        pid,
                    });
                }
            }
        }
        found
    }
}

#[cfg(target_os = "linux")]
fn overlaps(a: IpAddr, b: IpAddr) -> bool {
    if a.is_ipv4() == b.is_ipv4() {
        a == b || a.is_unspecified() || b.is_unspecified()
    }
    // An IPv6 wildcard may also accept IPv4; specific IPv6 addresses do not.
    else {
        a == IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED)
            || b == IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED)
    }
}

#[cfg(target_os = "linux")]
fn proc_address(text: &str) -> Option<SocketAddr> {
    let (ip, port) = text.split_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    let ip = if ip.len() == 8 {
        IpAddr::from(std::net::Ipv4Addr::from(
            u32::from_str_radix(ip, 16).ok()?.to_ne_bytes(),
        ))
    } else if ip.len() == 32 {
        let mut bytes = [0; 16];
        for (index, chunk) in ip.as_bytes().chunks(8).enumerate() {
            let value = u32::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
            bytes[index * 4..index * 4 + 4].copy_from_slice(&value.to_ne_bytes());
        }
        IpAddr::from(std::net::Ipv6Addr::from(bytes))
    } else {
        return None;
    };
    Some(SocketAddr::new(ip, port))
}

#[cfg(target_os = "linux")]
fn owner(inode: &str) -> Option<(u32, String)> {
    if inode == "0" {
        return None;
    }
    let target = format!("socket:[{inode}]");
    for process in std::fs::read_dir("/proc").ok()?.flatten() {
        let Ok(pid) = process.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(fds) = std::fs::read_dir(process.path().join("fd")) else {
            continue;
        };
        if fds.flatten().any(|fd| {
            std::fs::read_link(fd.path()).is_ok_and(|link| link.as_os_str() == target.as_str())
        }) {
            let name = std::fs::read_to_string(process.path().join("comm"))
                .ok()?
                .trim()
                .to_owned();
            return Some((pid, name));
        }
    }
    None
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn proc_socket_decoding_preserves_loopback_and_port() {
        assert_eq!(
            proc_address("3500007F:0035"),
            Some("127.0.0.53:53".parse().unwrap())
        );
        assert_eq!(
            proc_address("00000000000000000000000001000000:041D"),
            Some("[::1]:1053".parse().unwrap())
        );
    }

    #[test]
    fn distinct_loopback_addresses_and_specific_ipv6_listeners_do_not_conflict() {
        assert!(!overlaps(
            "127.0.0.53".parse().unwrap(),
            "127.0.0.1".parse().unwrap()
        ));
        assert!(!overlaps(
            "::1".parse().unwrap(),
            "127.0.0.1".parse().unwrap()
        ));
        assert!(overlaps(
            "127.0.0.53".parse().unwrap(),
            "0.0.0.0".parse().unwrap()
        ));
        assert!(overlaps(
            "::".parse().unwrap(),
            "127.0.0.1".parse().unwrap()
        ));
    }

    #[test]
    fn a_busy_dns_listener_offers_an_alternative_and_preserves_the_owner() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let config =
            Config::from_value(json!({"dns": {"enable": true, "listen": address.to_string()}}))
                .unwrap();
        let conflict = dns_conflict(&config).unwrap();
        assert!(
            conflict
                .owners
                .iter()
                .any(|owner| owner.pid == Some(std::process::id()) && owner.address == address)
        );
        let replacement: SocketAddr = conflict.replacement.parse().unwrap();
        assert_ne!(replacement, address);
        assert!(replacement.ip().is_loopback());
        assert!(std::net::TcpStream::connect(address).is_ok());
        assert!(bindable(replacement));
    }
}
