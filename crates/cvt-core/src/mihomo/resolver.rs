//! Link-scoped systemd-resolved integration for the application's private TUN.
use std::io::{Read as _, Seek as _};
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::model::config::Config;
use crate::{Error, Result};

/// Interface reserved for application-managed TUN routing and DNS policy.
pub const DEVICE: &str = "cvt-mihomo";
/// Only these resolved actions are delegated by explicit authorization.
pub const ACTIONS: &[&str] = &[
    "org.freedesktop.resolve1.set-dns-servers",
    "org.freedesktop.resolve1.set-domains",
    "org.freedesktop.resolve1.set-default-route",
    "org.freedesktop.resolve1.revert",
];

/// DNS listener actually enabled in the generated runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// A local listener; wildcard binds are contacted through loopback.
    pub server: SocketAddr,
}

impl Target {
    /// Resolve a TUN configuration to a local DNS endpoint.
    ///
    /// # Errors
    /// Refuses disabled DNS, foreign interfaces, invalid/nonlocal listeners.
    pub fn from_config(config: &Config) -> Result<Option<Self>> {
        if !cfg!(target_os = "linux") || !config.tun_enabled() {
            return Ok(None);
        }
        let value = config.as_value();
        if value["tun"]["device"].as_str() != Some(DEVICE) {
            return Err(Error::invalid(
                "TUN DNS",
                "application TUN must use cvt-mihomo; regenerate the runtime configuration",
            ));
        }
        if value["dns"]["enable"].as_bool() != Some(true) {
            return Err(Error::invalid(
                "TUN DNS",
                "system DNS integration requires dns.enable: true",
            ));
        }
        let listen = value["dns"]["listen"]
            .as_str()
            .ok_or_else(|| Error::invalid("TUN DNS", "a local dns.listen endpoint is required"))?;
        let normalized = if listen.starts_with(':') {
            format!("0.0.0.0{listen}")
        } else {
            listen.to_owned()
        };
        let mut server: SocketAddr = normalized
            .parse()
            .map_err(|_| Error::invalid("TUN DNS", "dns.listen must be an IP address and port"))?;
        if server.ip().is_unspecified() {
            server.set_ip(if server.is_ipv4() {
                IpAddr::from([127, 0, 0, 1])
            } else {
                IpAddr::from(std::net::Ipv6Addr::LOCALHOST)
            });
        }
        if !server.ip().is_loopback() || server.port() == 0 {
            return Err(Error::invalid(
                "TUN DNS",
                "use a loopback DNS listener such as 127.0.0.1:1053",
            ));
        }
        if server.ip() == IpAddr::from([127, 0, 0, 53])
            || server.ip() == IpAddr::from([127, 0, 0, 54])
        {
            return Err(Error::invalid(
                "TUN DNS",
                "the DNS listener must not point back to systemd-resolved's stub",
            ));
        }
        for key in [
            "nameserver",
            "fallback",
            "default-nameserver",
            "proxy-server-nameserver",
            "direct-nameserver",
            "nameserver-policy",
        ] {
            if recursive_upstream(&value["dns"][key], server) {
                return Err(Error::invalid(
                    "TUN DNS",
                    format!(
                        "dns.{key} points back to the system resolver or Mihomo DNS listener; use independent upstream DNS servers"
                    ),
                ));
            }
        }
        Ok(Some(Self { server }))
    }
}

/// Whether systemd-resolved is active; never starts or enables a service.
#[must_use]
pub fn available() -> bool {
    cfg!(target_os = "linux")
        && Path::new("/usr/bin/resolvectl").is_file()
        && command(
            Command::new("/usr/bin/systemctl").args(["is-active", "systemd-resolved.service"]),
            Duration::from_secs(3),
        )
        .is_ok()
}

/// Check authorization without an agent or password prompt.
#[must_use]
pub fn authorized() -> bool {
    !available()
        || !Path::new("/usr/bin/pkcheck").is_file()
        || ACTIONS.iter().all(|action| {
            command(
                Command::new("/usr/bin/pkcheck").args([
                    "--action-id",
                    action,
                    "--process",
                    &std::process::id().to_string(),
                ]),
                Duration::from_secs(3),
            )
            .is_ok()
        })
}

/// Root-installed policy scoped to the approved user and DNS routing actions.
#[must_use]
pub fn policy(user: &str) -> String {
    let user = serde_json::to_string(user).expect("string serialization");
    let actions = serde_json::to_string(ACTIONS).expect("action serialization");
    format!(
        "// clash-verge-tui: explicitly authorized Link-level DNS only.\npolkit.addRule(function(action, subject) {{\n  if (subject.user === {user} && {actions}.indexOf(action.id) >= 0) return polkit.Result.YES;\n}});\n"
    )
}

/// Execute a noninteractive system utility with bounded time and output.
///
/// # Errors
/// Returns timeout, execution, or nonzero exit details.
pub fn command(command: &mut Command, timeout: Duration) -> Result<String> {
    let mut output = tempfile::tempfile().map_err(|e| Error::io("resolver output", e))?;
    let error_output = output
        .try_clone()
        .map_err(|e| Error::io("resolver output", e))?;
    let mut child = command
        .env("LC_ALL", "C")
        .env("SYSTEMD_COLORS", "0")
        .env("SYSTEMD_PAGER", "cat")
        .stdin(Stdio::null())
        .stdout(Stdio::from(
            output
                .try_clone()
                .map_err(|e| Error::io("resolver output", e))?,
        ))
        .stderr(Stdio::from(error_output))
        .spawn()
        .map_err(|e| Error::invalid("system DNS", e.to_string()))?;
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::invalid("system DNS", "system utility timed out"));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::invalid("system DNS", error.to_string()));
            }
        }
    };
    output
        .rewind()
        .map_err(|e| Error::io("resolver output", e))?;
    let mut bytes = Vec::new();
    output
        .take(65_536)
        .read_to_end(&mut bytes)
        .map_err(|e| Error::io("resolver output", e))?;
    let text = String::from_utf8_lossy(&bytes).trim().to_owned();
    if status.success() {
        Ok(text)
    } else {
        Err(Error::invalid(
            "system DNS",
            format!("{text}; authorize Link-level DNS in the TUI before starting TUN"),
        ))
    }
}

pub(super) fn resolvectl(args: &[&str]) -> Result<String> {
    command(
        Command::new("/usr/bin/resolvectl")
            .arg("--no-ask-password")
            .args(args),
        Duration::from_secs(5),
    )
}

fn recursive_upstream(value: &serde_json::Value, listener: SocketAddr) -> bool {
    match value {
        serde_json::Value::Array(items) => {
            items.iter().any(|item| recursive_upstream(item, listener))
        }
        serde_json::Value::Object(items) => items
            .values()
            .any(|item| recursive_upstream(item, listener)),
        serde_json::Value::String(text) => {
            if text == "system" || text.starts_with("system://") {
                return true;
            }
            let address = text
                .parse::<SocketAddr>()
                .ok()
                .or_else(|| {
                    text.parse::<IpAddr>()
                        .ok()
                        .map(|ip| SocketAddr::new(ip, 53))
                })
                .or_else(|| {
                    reqwest::Url::parse(text).ok().and_then(|url| {
                        let ip = url
                            .host_str()?
                            .trim_matches(['[', ']'])
                            .parse::<IpAddr>()
                            .ok()?;
                        Some(SocketAddr::new(
                            ip,
                            url.port_or_known_default().unwrap_or(53),
                        ))
                    })
                });
            address.is_some_and(|address| address == listener || (address.port() == 53 && matches!(address.ip(), IpAddr::V4(ip) if ip.octets() == [127,0,0,53] || ip.octets() == [127,0,0,54])))
        }
        _ => false,
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use serde_json::json;

    fn tun_config(listen: &str, upstream: &str) -> Config {
        Config::from_value(json!({
            "tun": { "enable": true, "device": DEVICE },
            "dns": { "enable": true, "listen": listen, "nameserver": [upstream] }
        }))
        .unwrap()
    }

    #[test]
    fn wildcard_port_53_is_a_valid_local_dns_target() {
        assert_eq!(
            Target::from_config(&tun_config(":53", "1.1.1.1"))
                .unwrap()
                .unwrap()
                .server,
            "127.0.0.1:53".parse::<SocketAddr>().unwrap()
        );
    }

    #[test]
    fn resolver_handoff_rejects_disabled_dns_and_foreign_links() {
        let mut value = tun_config("127.0.0.1:1053", "1.1.1.1").as_value();
        value["dns"]["enable"] = json!(false);
        assert!(Target::from_config(&Config::from_value(value.clone()).unwrap()).is_err());
        value["dns"]["enable"] = json!(true);
        value["tun"]["device"] = json!("eth0");
        assert!(Target::from_config(&Config::from_value(value).unwrap()).is_err());
    }

    #[test]
    fn system_and_listener_upstreams_cannot_form_a_dns_loop() {
        for upstream in [
            "system",
            "system://",
            "127.0.0.53",
            "udp://127.0.0.54:53",
            "127.0.0.1:1053",
            "tcp://127.0.0.1:1053",
        ] {
            assert!(
                Target::from_config(&tun_config("127.0.0.1:1053", upstream)).is_err(),
                "{upstream}"
            );
        }
        let mut value = tun_config("127.0.0.1:1053", "1.1.1.1").as_value();
        value["dns"]["nameserver-policy"] = json!({"example.com": ["system"]});
        assert!(Target::from_config(&Config::from_value(value).unwrap()).is_err());
    }

    #[test]
    fn policy_escapes_user_names_and_scopes_all_actions() {
        let policy = policy("a\"; malicious()");
        assert!(policy.contains("subject.user === \"a\\\"; malicious()\""));
        for action in ACTIONS {
            assert!(policy.contains(action));
        }
        assert!(!policy.contains("org.freedesktop.resolve1.set-dnssec"));
    }

    #[test]
    fn system_commands_reject_unsuccessful_exit_and_obey_a_deadline() {
        assert!(
            command(
                Command::new("/bin/sh").args(["-c", "echo failed; exit 7"]),
                Duration::from_secs(1)
            )
            .unwrap_err()
            .to_string()
            .contains("failed")
        );
        let started = Instant::now();
        assert!(
            command(
                Command::new("/bin/sh").args(["-c", "exec sleep 10"]),
                Duration::from_millis(100)
            )
            .unwrap_err()
            .to_string()
            .contains("timed out")
        );
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
