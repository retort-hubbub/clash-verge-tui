//! Where the core's API lives, and how to reach it.
//!
//! mihomo can expose its controller on four transports at once, with *different*
//! authentication behaviour:
//!
//! | Config key | Transport | Honours `secret` |
//! |---|---|---|
//! | `external-controller` | TCP | yes |
//! | `external-controller-tls` | TLS | yes |
//! | `external-controller-unix` | unix socket | **no** |
//! | `external-controller-pipe` | Windows named pipe | **no** |
//!
//! The unix socket and pipe listeners are built with an empty secret, so
//! authentication is not merely optional there — it is absent. [`Endpoint`]
//! models this honestly rather than pretending a secret protects every
//! transport, and [`Endpoint::is_authenticated`] lets callers warn about it.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Which transport to use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum Transport {
    /// A plain TCP address such as `127.0.0.1:9090`.
    Tcp(String),
    /// A TLS listener address.
    Tls(String),
    /// A unix domain socket path.
    #[serde(rename = "unix")]
    Unix(PathBuf),
    /// A Windows named pipe such as `\\.\pipe\mihomo`.
    #[serde(rename = "pipe")]
    Pipe(String),
}

/// A controller endpoint plus its credentials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoint {
    /// Transport to dial.
    pub transport: Transport,
    /// Shared secret, when the transport checks one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
}

impl Endpoint {
    /// A TCP endpoint.
    #[must_use]
    pub fn tcp(addr: impl Into<String>, secret: Option<String>) -> Self {
        Self {
            transport: Transport::Tcp(addr.into()),
            secret: normalise(secret),
        }
    }

    /// A TLS controller endpoint.
    #[must_use]
    pub fn tls(addr: impl Into<String>, secret: Option<String>) -> Self {
        Self {
            transport: Transport::Tls(addr.into()),
            secret: normalise(secret),
        }
    }

    /// A unix socket endpoint. Secrets are accepted but never sent, because the
    /// core does not check them on this transport.
    #[must_use]
    pub fn unix(path: impl Into<PathBuf>) -> Self {
        Self {
            transport: Transport::Unix(path.into()),
            secret: None,
        }
    }

    /// A Windows named pipe endpoint.
    #[must_use]
    pub fn pipe(name: impl Into<String>) -> Self {
        Self {
            transport: Transport::Pipe(name.into()),
            secret: None,
        }
    }

    /// Parse the four `external-controller*`-style keys out of a config.
    ///
    /// Prefers TCP, then TLS, then the unix socket, then the pipe — the order
    /// that gives the most capable transport first.
    #[must_use]
    pub fn from_config(config: &crate::model::config::Config) -> Option<Self> {
        let secret = config.secret();
        if let Some(addr) = config
            .get_str("external-controller")
            .filter(|s| !s.is_empty())
        {
            return Some(Self::tcp(addr, secret));
        }
        if let Some(addr) = config
            .get_str("external-controller-tls")
            .filter(|s| !s.is_empty())
        {
            return Some(Self::tls(addr, secret));
        }
        if let Some(path) = config
            .get_str("external-controller-unix")
            .filter(|s| !s.is_empty())
        {
            return Some(Self::unix(PathBuf::from(path)));
        }
        config
            .get_str("external-controller-pipe")
            .filter(|s| !s.is_empty())
            .map(Self::pipe)
    }

    /// The `http://` base URL requests are built against.
    ///
    /// For socket transports the host is a placeholder: `reqwest` routes every
    /// request through the configured socket and never resolves it.
    #[must_use]
    pub fn base_url(&self) -> String {
        match &self.transport {
            Transport::Tcp(addr) => format!("http://{addr}"),
            Transport::Tls(addr) => format!("https://{addr}"),
            Transport::Unix(_) | Transport::Pipe(_) => "http://localhost".to_owned(),
        }
    }

    /// The `ws://` URL for a streaming path, e.g. `drive://logs`.
    ///
    /// `path` must start with `/`.
    #[must_use]
    pub fn ws_url(&self, path: &str) -> String {
        let p = if path.starts_with('/') {
            path.to_owned()
        } else {
            format!("/{path}")
        };
        match &self.transport {
            Transport::Tcp(addr) => format!("ws://{addr}{p}"),
            Transport::Tls(addr) => format!("wss://{addr}{p}"),
            Transport::Unix(_) | Transport::Pipe(_) => format!("ws://localhost{p}"),
        }
    }

    /// Whether the transport actually checks the secret.
    ///
    /// `false` for unix sockets and named pipes: mihomo builds those listeners
    /// with an empty secret, so anything that can open the socket is trusted.
    #[must_use]
    pub fn is_authenticated(&self) -> bool {
        matches!(self.transport, Transport::Tcp(_) | Transport::Tls(_)) && self.secret.is_some()
    }

    /// Human-readable description used in status bars and errors.
    #[must_use]
    pub fn describe(&self) -> String {
        match &self.transport {
            Transport::Tcp(a) => format!("tcp://{a}"),
            Transport::Tls(a) => format!("tls://{a}"),
            Transport::Unix(p) => format!("unix://{}", p.display()),
            Transport::Pipe(p) => format!("pipe://{p}"),
        }
    }

    /// `true` when the endpoint is only reachable from this machine.
    ///
    /// The core binds `127.0.0.1` by default; anything else is a deliberate
    /// decision worth surfacing in the UI.
    #[must_use]
    pub fn is_loopback(&self) -> bool {
        match &self.transport {
            Transport::Unix(_) | Transport::Pipe(_) => true,
            Transport::Tcp(addr) | Transport::Tls(addr) => {
                let host = addr.rsplit_once(':').map_or(addr.as_str(), |(h, _)| h);
                let host = host.trim_matches(['[', ']']);
                host == "localhost"
                    || host == "::1"
                    || host
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|ip| ip.is_loopback())
            }
        }
    }

    /// Validate that the endpoint is usable before dialling.
    ///
    /// # Errors
    /// [`Error::InvalidValue`] for a malformed address or a missing socket.
    pub fn validate(&self) -> Result<()> {
        match &self.transport {
            Transport::Tcp(addr) | Transport::Tls(addr) => {
                let (host, port) = addr.rsplit_once(':').ok_or_else(|| {
                    Error::invalid(
                        "external-controller",
                        format!("`{addr}` is missing a port; expected `host:port`"),
                    )
                })?;
                if host.is_empty() {
                    return Err(Error::invalid(
                        "external-controller",
                        format!("`{addr}` has an empty host"),
                    ));
                }
                port.parse::<u16>().map_err(|_| {
                    Error::invalid(
                        "external-controller",
                        format!("`{port}` is not a valid port"),
                    )
                })?;
                Ok(())
            }
            Transport::Unix(path) => {
                if path.as_os_str().is_empty() {
                    return Err(Error::invalid("external-controller-unix", "path is empty"));
                }
                Ok(())
            }
            Transport::Pipe(name) => {
                if !name.starts_with(r"\\.\pipe\") {
                    return Err(Error::invalid(
                        "external-controller-pipe",
                        "a named pipe must start with `\\\\.\\pipe\\`",
                    ));
                }
                Ok(())
            }
        }
    }
}

fn normalise(secret: Option<String>) -> Option<String> {
    secret
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::model::config::Config;

    #[test]
    fn builds_base_and_ws_urls_for_every_transport() {
        let t = Endpoint::tcp("127.0.0.1:9090", Some("s".into()));
        assert_eq!(t.base_url(), "http://127.0.0.1:9090");
        assert_eq!(t.ws_url("/logs"), "ws://127.0.0.1:9090/logs");
        assert_eq!(t.ws_url("traffic"), "ws://127.0.0.1:9090/traffic");

        let tls = Endpoint::tls("localhost:9443", Some("s".into()));
        assert_eq!(tls.base_url(), "https://localhost:9443");
        assert_eq!(tls.ws_url("/logs"), "wss://localhost:9443/logs");
        assert!(tls.is_authenticated());
        assert!(tls.is_loopback());

        let u = Endpoint::unix("/run/mihomo.sock");
        assert_eq!(u.base_url(), "http://localhost");
        assert_eq!(u.ws_url("/logs"), "ws://localhost/logs");
    }

    #[test]
    fn knows_which_transports_ignore_the_secret() {
        assert!(Endpoint::tcp("127.0.0.1:9090", Some("s".into())).is_authenticated());
        // A TCP endpoint with no secret is open, exactly like a socket.
        assert!(!Endpoint::tcp("127.0.0.1:9090", None).is_authenticated());
        assert!(!Endpoint::tcp("127.0.0.1:9090", Some("  ".into())).is_authenticated());
        // The core builds socket listeners with an empty secret.
        assert!(!Endpoint::unix("/run/mihomo.sock").is_authenticated());
        assert!(!Endpoint::pipe(r"\\.\pipe\mihomo").is_authenticated());
    }

    #[test]
    fn reads_endpoints_out_of_a_config_in_preference_order() {
        let c = Config::from_yaml(
            "external-controller: 127.0.0.1:9090\nsecret: hunter2\nexternal-controller-unix: /run/m.sock\n",
        )
        .unwrap();
        let e = Endpoint::from_config(&c).unwrap();
        assert_eq!(e, Endpoint::tcp("127.0.0.1:9090", Some("hunter2".into())));

        let c = Config::from_yaml("external-controller-unix: /run/m.sock\n").unwrap();
        let e = Endpoint::from_config(&c).unwrap();
        assert_eq!(e, Endpoint::unix("/run/m.sock"));

        let c = Config::from_yaml("external-controller-tls: localhost:9443\nsecret: s\n").unwrap();
        assert_eq!(
            Endpoint::from_config(&c),
            Some(Endpoint::tls("localhost:9443", Some("s".into())))
        );

        let c = Config::from_yaml("mode: rule\n").unwrap();
        assert!(Endpoint::from_config(&c).is_none());
    }

    #[test]
    fn detects_loopback_binding() {
        assert!(Endpoint::tcp("127.0.0.1:9090", None).is_loopback());
        assert!(Endpoint::tcp("localhost:9090", None).is_loopback());
        assert!(Endpoint::tcp("[::1]:9090", None).is_loopback());
        assert!(!Endpoint::tcp("0.0.0.0:9090", None).is_loopback());
        assert!(!Endpoint::tcp("192.168.1.5:9090", None).is_loopback());
        assert!(Endpoint::unix("/run/m.sock").is_loopback());
    }

    #[test]
    fn validates_addresses() {
        assert!(Endpoint::tcp("127.0.0.1:9090", None).validate().is_ok());
        assert!(Endpoint::tcp("127.0.0.1", None).validate().is_err());
        assert!(Endpoint::tcp(":9090", None).validate().is_err());
        assert!(
            Endpoint::tcp("127.0.0.1:notaport", None)
                .validate()
                .is_err()
        );
        assert!(Endpoint::tcp("127.0.0.1:70000", None).validate().is_err());
        assert!(Endpoint::unix("/run/m.sock").validate().is_ok());
        assert!(Endpoint::pipe(r"\\.\pipe\mihomo").validate().is_ok());
        assert!(Endpoint::pipe("mihomo").validate().is_err());
    }
}
