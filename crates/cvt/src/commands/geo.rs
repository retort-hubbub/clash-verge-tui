//! `geo` — where the traffic actually comes out.
//!
//! The one question a proxy is for, and the one a latency number does not
//! answer: a node can be fast and in the wrong country, or fast and in a
//! datacentre the sites you want have already blocked. This asks an IP-info
//! service *through the core's own proxy port*, so the answer describes the
//! tunnel the user gets rather than the machine the program is running on.
//!
//! `--direct` asks the same question without the proxy, and the two answers
//! side by side are the whole point: the interesting fact is not your address
//! or the node's, it is that they differ.

use std::time::Duration;

use anyhow::Result;
use cvt_core::error::Error;
use cvt_core::model::config::Config;
use serde::Serialize;

use crate::cli::GeoArgs;
use crate::context::Ctx;
use crate::output::{Fields, Output, Report};

/// Where the answers come from, in the order they are tried.
///
/// More than one because they are other people's services: one being down, or
/// refusing a request from a given country, is not a reason for this command to
/// have nothing to say. The plain-text one is last and is the only one that
/// needs no JSON parsing, so it works when a service has changed its shape.
const SOURCES: &[&str] = &[
    "https://ipinfo.io/json",
    "https://api.ip.sb/geoip",
    "https://1.1.1.1/cdn-cgi/trace",
];

/// What `geo` found.
#[derive(Debug, Serialize)]
pub struct GeoReport {
    /// The address the outside world sees.
    pub ip: String,
    /// Two-letter country code, when the service gave one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    /// Region or province.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    /// City.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
    /// The network operator or hosting provider.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub org: Option<String>,
    /// Which endpoint answered.
    pub source: String,
    /// How the request was made: through the core, or directly.
    pub via: String,
    /// Round trip in milliseconds.
    pub delay_ms: u64,
    /// The library's one-line summary.
    pub summary: String,
}

impl Report for GeoReport {
    fn schema(&self) -> &'static str {
        "cvt.geo.v1"
    }

    fn render(&self, _out: Output) -> String {
        let mut fields = Fields::new();
        fields.push("ip", self.ip.clone());
        for (label, value) in [
            ("country", &self.country),
            ("region", &self.region),
            ("city", &self.city),
            ("org", &self.org),
        ] {
            fields.push_opt(label, value.clone());
        }
        fields.push("via", self.via.clone());
        fields.push("delay", format!("{} ms", self.delay_ms));
        fields.push("source", self.source.clone());
        fields.render()
    }
}

/// Run `geo`.
///
/// # Errors
/// [`Error::InvalidValue`] when there is no generated configuration to read a
/// proxy port from, and [`Error::Http`] when every source failed.
pub async fn run(ctx: &Ctx, args: &GeoArgs) -> Result<()> {
    let timeout = Duration::from_millis(args.timeout.unwrap_or(10_000));
    let (client, via) = if args.direct {
        (build(None, timeout)?, "direct".to_owned())
    } else {
        let port = proxy_port(ctx)?;
        (
            build(Some(port), timeout)?,
            format!("the core's proxy on 127.0.0.1:{port}"),
        )
    };

    let started = std::time::Instant::now();
    let mut failures = Vec::new();
    for source in SOURCES {
        match fetch(&client, source).await {
            Ok(found) => {
                let delay_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                let summary = format!("{} via {via}", found.ip);
                return ctx.out().emit(&GeoReport {
                    ip: found.ip,
                    country: found.country,
                    region: found.region,
                    city: found.city,
                    org: found.org,
                    source: (*source).to_owned(),
                    via,
                    delay_ms,
                    summary,
                });
            }
            Err(error) => failures.push(format!("{source}: {error}")),
        }
    }
    Err(Error::http(
        SOURCES.join(", "),
        std::io::Error::other(format!(
            "no source answered through {via}: {}",
            failures.join("; ")
        )),
    )
    .into())
}

/// What one source said.
#[derive(Debug, Default)]
struct Found {
    ip: String,
    country: Option<String>,
    region: Option<String>,
    city: Option<String>,
    org: Option<String>,
}

/// A client pointed at the core's proxy port, or at nothing.
fn build(proxy_port: Option<u16>, timeout: Duration) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .timeout(timeout)
        .user_agent(concat!("clash-verge-tui/", env!("CARGO_PKG_VERSION")));
    if let Some(port) = proxy_port {
        builder = builder.proxy(reqwest::Proxy::all(format!("http://127.0.0.1:{port}"))?);
    }
    builder.build().map_err(|e| Error::http("geo", e).into())
}

/// The port the core is listening on, from the generated configuration.
fn proxy_port(ctx: &Ctx) -> Result<u16> {
    let path = ctx.paths().runtime_config();
    if !path.is_file() {
        return Err(Error::invalid(
            "runtime config",
            format!(
                "{} does not exist yet; run `clash-verge-tui config generate --apply`",
                path.display()
            ),
        )
        .into());
    }
    let yaml = ctx.paths().read(&path)?;
    let config = Config::from_yaml(&yaml)?;
    config.effective_proxy_port().ok_or_else(|| {
        Error::invalid(
            "mixed-port",
            "the generated configuration has no `mixed-port`, `port` or `socks-port`, \
             so there is no proxy to ask through; use `--direct` for this machine's own \
             address",
        )
        .into()
    })
}

/// Ask one source, and read whatever shape it answers in.
async fn fetch(client: &reqwest::Client, source: &str) -> Result<Found> {
    let response = client
        .get(source)
        .header(reqwest::header::ACCEPT, "application/json, text/plain")
        .send()
        .await
        .map_err(|e| Error::http(source, e))?;
    if !response.status().is_success() {
        return Err(Error::http(
            source,
            std::io::Error::other(format!("HTTP {}", response.status())),
        )
        .into());
    }
    let body = response.text().await.map_err(|e| Error::http(source, e))?;
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) {
        return Ok(from_json(&value));
    }
    Ok(from_trace(&body))
}

/// The three JSON shapes these services use.
fn from_json(value: &serde_json::Value) -> Found {
    let text = |key: &str| {
        value
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    Found {
        ip: text("ip").or_else(|| text("query")).unwrap_or_default(),
        country: text("country").or_else(|| text("country_code")),
        region: text("region").or_else(|| text("region_name")),
        city: text("city"),
        // `org` is ipinfo's, `isp` is ip.sb's; either answers the same question.
        org: text("org").or_else(|| text("isp")).or_else(|| text("asn")),
    }
}

/// `https://1.1.1.1/cdn-cgi/trace` answers `key=value` lines.
fn from_trace(body: &str) -> Found {
    let field = |key: &str| {
        body.lines()
            .find_map(|line| line.strip_prefix(&format!("{key}=")))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    Found {
        ip: field("ip").unwrap_or_default(),
        country: field("loc"),
        region: None,
        city: None,
        org: field("colo").map(|colo| format!("Cloudflare {colo}")),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn each_json_shape_is_read() {
        let ipinfo = serde_json::json!({
            "ip": "203.0.113.7", "city": "Tokyo", "region": "Tokyo",
            "country": "JP", "org": "AS1234 Example"
        });
        let found = from_json(&ipinfo);
        assert_eq!(found.ip, "203.0.113.7");
        assert_eq!(found.city.as_deref(), Some("Tokyo"));
        assert_eq!(found.org.as_deref(), Some("AS1234 Example"));

        // ip.sb spells the same fields differently, and has no `org`.
        let ip_sb = serde_json::json!({
            "ip": "198.51.100.9", "country_code": "SG", "isp": "Example Pte"
        });
        let found = from_json(&ip_sb);
        assert_eq!(found.country.as_deref(), Some("SG"));
        assert_eq!(found.org.as_deref(), Some("Example Pte"));

        // A shape neither of them uses must not panic, and must not invent an
        // address: an empty `ip` is the caller's cue to try the next source.
        assert_eq!(from_json(&serde_json::json!({"ip": "  "})).ip, "");
        assert_eq!(from_json(&serde_json::json!({})).ip, "");
    }

    #[test]
    fn the_trace_shape_is_read() {
        let body = "fl=1a2\nip=203.0.113.7\nts=1\nloc=JP\ncolo=NRT\n";
        let found = from_trace(body);
        assert_eq!(found.ip, "203.0.113.7");
        assert_eq!(found.country.as_deref(), Some("JP"));
        assert_eq!(found.org.as_deref(), Some("Cloudflare NRT"));
        // And a body that is not a trace at all yields nothing rather than
        // something made up.
        assert_eq!(from_trace("<html>nope</html>").ip, "");
    }
}
