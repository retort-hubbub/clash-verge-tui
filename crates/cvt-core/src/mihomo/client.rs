//! The mihomo controller client.
//!
//! One type, [`Client`], wraps every REST call the application makes. It is
//! deliberately thin: it encodes the endpoint, the `Bearer` header and the
//! error envelope once, and exposes one typed method per operation. Nothing
//! here decides *what* to call — that is the UI's and the CLI's job.
//!
//! # Error decoding
//!
//! The core distinguishes two kinds of "not found", and so does this client:
//!
//! * a JSON body `{"message":"Resource not found"}` means the route exists but
//!   the named object does not;
//! * a `text/plain` body `404 page not found` means the **route is absent from
//!   this build**, which is how embedded builds omit `/configs`, `/restart` and
//!   friends.
//!
//! [`Client::probe`] uses that difference to report which features the running
//! core actually supports, instead of discovering it through a failed action.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::error::{Error, Result};
use crate::mihomo::endpoint::Endpoint;
use crate::mihomo::types::{
    ApiMessage, ConfigPatch, ConnectionsResponse, DelayResponse, GeneralConfig, GroupDelay, Hello,
    ProxiesResponse, ProxyProviderInfo, ProxyProvidersResponse, ProxyView, RuleInfo,
    RuleProviderInfo, RuleProvidersResponse, RulesResponse, StatusOk, Version,
};

/// Default request timeout for ordinary calls.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// Extra time allowed on top of a latency test's own timeout.
const DELAY_SLACK: Duration = Duration::from_secs(5);

/// A controller API client.
#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    endpoint: Endpoint,
}

impl Client {
    /// Build a client for an endpoint.
    ///
    /// # Errors
    /// [`Error::InvalidValue`] if the endpoint is malformed, or
    /// [`Error::ControllerUnreachable`] if the HTTP backend cannot be built.
    pub fn new(endpoint: Endpoint) -> Result<Self> {
        endpoint.validate()?;
        let http = build_http(&endpoint, DEFAULT_TIMEOUT)?;
        Ok(Self { http, endpoint })
    }

    /// Build a client with a custom default timeout.
    ///
    /// # Errors
    /// As [`Client::new`].
    pub fn with_timeout(endpoint: Endpoint, timeout: Duration) -> Result<Self> {
        endpoint.validate()?;
        let http = build_http(&endpoint, timeout)?;
        Ok(Self { http, endpoint })
    }

    /// The endpoint this client talks to.
    #[must_use]
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    // ------------------------------------------------------------- plumbing

    fn url(&self, path: &str) -> String {
        let base = self.endpoint.base_url();
        if path.starts_with('/') {
            format!("{base}{path}")
        } else {
            format!("{base}/{path}")
        }
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let mut req = self
            .http
            .request(method, self.url(path))
            .header(reqwest::header::ACCEPT, "application/json");
        if let Some(secret) = &self.endpoint.secret {
            req = req.bearer_auth(secret);
        }
        req
    }

    async fn send<T: DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        kind: &'static str,
        body: Option<Value>,
    ) -> Result<T> {
        let mut req = self.request(method.clone(), path);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let response = req.send().await.map_err(|e| Error::ControllerUnreachable {
            endpoint: self.endpoint.describe(),
            source: Box::new(e),
        })?;
        let method_name = static_method_name(&method);
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|e| Error::ControllerUnreachable {
                endpoint: self.endpoint.describe(),
                source: Box::new(e),
            })?;

        if !status.is_success() {
            return Err(decode_error(method_name, path, status.as_u16(), &bytes));
        }
        if bytes.is_empty() {
            return Err(Error::Api {
                method: method_name,
                path: path.to_owned(),
                status: status.as_u16(),
                body: format!("expected a {kind} body but the response was empty"),
            });
        }
        serde_json::from_slice(&bytes).map_err(|e| Error::Parse {
            kind,
            path: path.into(),
            source: Box::new(e),
        })
    }

    /// Send a request whose only success signal is the status code.
    async fn send_no_content(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<()> {
        let mut req = self.request(method.clone(), path);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let response = req.send().await.map_err(|e| Error::ControllerUnreachable {
            endpoint: self.endpoint.describe(),
            source: Box::new(e),
        })?;
        let method_name = static_method_name(&method);
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        let bytes = response.bytes().await.unwrap_or_default();
        Err(decode_error(method_name, path, status.as_u16(), &bytes))
    }

    // --------------------------------------------------------------- system

    /// `GET /` — cheap liveness check.
    ///
    /// # Errors
    /// Propagates transport and decode failures.
    pub async fn hello(&self) -> Result<Hello> {
        self.send(reqwest::Method::GET, "/", "hello", None).await
    }

    /// `GET /version`
    ///
    /// # Errors
    /// Propagates transport and decode failures.
    pub async fn version(&self) -> Result<Version> {
        self.send(reqwest::Method::GET, "/version", "version", None)
            .await
    }

    /// `true` when the controller answers at all.
    #[must_use]
    pub async fn is_reachable(&self) -> bool {
        self.hello().await.is_ok()
    }

    // -------------------------------------------------------------- proxies

    /// `GET /proxies` — every proxy and group, flat.
    ///
    /// # Errors
    /// Propagates transport and decode failures.
    pub async fn proxies(&self) -> Result<ProxiesResponse> {
        self.send(reqwest::Method::GET, "/proxies", "proxies", None)
            .await
    }

    /// `GET /proxies/{name}`
    ///
    /// # Errors
    /// [`Error::Api`] with status 404 when no such proxy exists.
    pub async fn proxy(&self, name: &str) -> Result<ProxyView> {
        let path = format!("/proxies/{}", encode_segment(name));
        self.send(reqwest::Method::GET, &path, "proxy", None).await
    }

    /// `GET /proxies/{name}/delay`
    ///
    /// `timeout` is milliseconds and is sent as an `int16` by the core, so it
    /// must not exceed 32767.
    ///
    /// # Errors
    /// [`Error::InvalidValue`] for an out-of-range timeout, otherwise
    /// propagates. A failed test surfaces as [`Error::Api`] with status 503.
    pub async fn proxy_delay(
        &self,
        name: &str,
        test_url: &str,
        timeout_ms: u32,
        expected: Option<&str>,
    ) -> Result<u16> {
        if timeout_ms == 0 || timeout_ms > crate::settings::MAX_TEST_TIMEOUT_MS {
            return Err(Error::invalid(
                "timeout",
                format!(
                    "{timeout_ms} ms is out of the 1..={} range the core accepts",
                    crate::settings::MAX_TEST_TIMEOUT_MS
                ),
            ));
        }
        let mut path = format!(
            "/proxies/{}/delay?url={}&timeout={timeout_ms}",
            encode_segment(name),
            encode_query(test_url)
        );
        if let Some(e) = expected.filter(|s| !s.is_empty()) {
            path.push_str("&expected=");
            path.push_str(&encode_query(e));
        }
        let r: DelayResponse = self
            .send_with_timeout(
                reqwest::Method::GET,
                &path,
                "delay",
                None,
                Duration::from_millis(u64::from(timeout_ms)) + DELAY_SLACK,
            )
            .await?;
        Ok(r.delay)
    }

    /// Like [`Client::send`] but with a per-call timeout.
    async fn send_with_timeout<T: DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        kind: &'static str,
        body: Option<Value>,
        timeout: Duration,
    ) -> Result<T> {
        let mut req = self.request(method.clone(), path).timeout(timeout);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let response = req.send().await.map_err(|e| Error::ControllerUnreachable {
            endpoint: self.endpoint.describe(),
            source: Box::new(e),
        })?;
        let method_name = static_method_name(&method);
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|e| Error::ControllerUnreachable {
                endpoint: self.endpoint.describe(),
                source: Box::new(e),
            })?;
        if !status.is_success() {
            return Err(decode_error(method_name, path, status.as_u16(), &bytes));
        }
        serde_json::from_slice(&bytes).map_err(|e| Error::Parse {
            kind,
            path: path.into(),
            source: Box::new(e),
        })
    }

    /// `PUT /proxies/{name}` — pin a group's selection.
    ///
    /// Works on `Selector`, `URLTest` and `Fallback`. `LoadBalance` and plain
    /// proxies answer `400 Must be a Selector`.
    ///
    /// # Errors
    /// [`Error::Api`] for an unknown member or a non-selectable target.
    pub async fn select(&self, group: &str, member: &str) -> Result<()> {
        let path = format!("/proxies/{}", encode_segment(group));
        self.send_no_content(
            reqwest::Method::PUT,
            &path,
            Some(serde_json::json!({ "name": member })),
        )
        .await
    }

    /// `DELETE /proxies/{name}` — clear a pinned selection.
    ///
    /// # Errors
    /// [`Error::Api`] with status 400 for a `Selector` or a plain proxy.
    pub async fn clear_selection(&self, group: &str) -> Result<()> {
        let path = format!("/proxies/{}", encode_segment(group));
        self.send_no_content(reqwest::Method::DELETE, &path, None)
            .await
    }

    // --------------------------------------------------------------- groups

    /// `GET /group` — policy groups only.
    ///
    /// # Errors
    /// Propagates transport and decode failures.
    pub async fn groups(&self) -> Result<Vec<ProxyView>> {
        let r: crate::mihomo::types::GroupsResponse = self
            .send(reqwest::Method::GET, "/group", "groups", None)
            .await?;
        Ok(r.proxies)
    }

    /// `GET /group/{name}`
    ///
    /// # Errors
    /// [`Error::Api`] with status 404 when the name is not a group.
    pub async fn group(&self, name: &str) -> Result<ProxyView> {
        let path = format!("/group/{}", encode_segment(name));
        self.send(reqwest::Method::GET, &path, "group", None).await
    }

    /// `GET /group/{name}/delay` — test every member.
    ///
    /// Members that fail are absent from the result, and a `0` entry is valid.
    ///
    /// # Errors
    /// [`Error::Api`] with status 504 when every member timed out.
    pub async fn group_delay(
        &self,
        group: &str,
        test_url: &str,
        timeout_ms: u32,
        expected: Option<&str>,
    ) -> Result<GroupDelay> {
        if timeout_ms == 0 || timeout_ms > crate::settings::MAX_TEST_TIMEOUT_MS {
            return Err(Error::invalid(
                "timeout",
                format!(
                    "{timeout_ms} ms is out of the 1..={} range the core accepts",
                    crate::settings::MAX_TEST_TIMEOUT_MS
                ),
            ));
        }
        let mut path = format!(
            "/group/{}/delay?url={}&timeout={timeout_ms}",
            encode_segment(group),
            encode_query(test_url)
        );
        if let Some(e) = expected.filter(|s| !s.is_empty()) {
            path.push_str("&expected=");
            path.push_str(&encode_query(e));
        }
        self.send_with_timeout(
            reqwest::Method::GET,
            &path,
            "group delay",
            None,
            Duration::from_millis(u64::from(timeout_ms)) + DELAY_SLACK,
        )
        .await
    }

    // ------------------------------------------------------------ providers

    /// `GET /providers/proxies`
    ///
    /// # Errors
    /// Propagates transport and decode failures.
    pub async fn proxy_providers(&self) -> Result<ProxyProvidersResponse> {
        self.send(
            reqwest::Method::GET,
            "/providers/proxies",
            "proxy providers",
            None,
        )
        .await
    }

    /// `GET /providers/proxies/{name}`
    ///
    /// # Errors
    /// [`Error::Api`] with status 404 for an unknown provider.
    pub async fn proxy_provider(&self, name: &str) -> Result<ProxyProviderInfo> {
        let path = format!("/providers/proxies/{}", encode_segment(name));
        self.send(reqwest::Method::GET, &path, "proxy provider", None)
            .await
    }

    /// `PUT /providers/proxies/{name}` — force a refresh.
    ///
    /// # Errors
    /// [`Error::Api`] with status 503 when the download fails.
    pub async fn update_proxy_provider(&self, name: &str) -> Result<()> {
        let path = format!("/providers/proxies/{}", encode_segment(name));
        self.send_no_content(reqwest::Method::PUT, &path, None)
            .await
    }

    /// `GET /providers/proxies/{name}/healthcheck` — starts a check and returns.
    ///
    /// # Errors
    /// Propagates transport failures.
    pub async fn proxy_provider_healthcheck(&self, name: &str) -> Result<()> {
        let path = format!("/providers/proxies/{}/healthcheck", encode_segment(name));
        self.send_no_content(reqwest::Method::GET, &path, None)
            .await
    }

    /// `GET /providers/rules`
    ///
    /// # Errors
    /// Propagates transport and decode failures.
    pub async fn rule_providers(&self) -> Result<RuleProvidersResponse> {
        self.send(
            reqwest::Method::GET,
            "/providers/rules",
            "rule providers",
            None,
        )
        .await
    }

    /// Look one rule provider up by name.
    ///
    /// The core has no `GET /providers/rules/{name}` — that path answers `405`
    /// — so the full collection is fetched and filtered here.
    ///
    /// # Errors
    /// [`Error::Api`] with status 404 when the provider is unknown, otherwise
    /// propagates.
    pub async fn rule_provider(&self, name: &str) -> Result<RuleProviderInfo> {
        let all = self.rule_providers().await?;
        all.providers
            .into_iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v)
            .ok_or_else(|| Error::Api {
                method: "GET",
                path: "/providers/rules".to_owned(),
                status: 404,
                body: format!("no rule provider named `{name}`"),
            })
    }

    /// `PUT /providers/rules/{name}` — force a refresh.
    ///
    /// # Errors
    /// [`Error::Api`] with status 503 when the download fails.
    pub async fn update_rule_provider(&self, name: &str) -> Result<()> {
        let path = format!("/providers/rules/{}", encode_segment(name));
        self.send_no_content(reqwest::Method::PUT, &path, None)
            .await
    }

    // ---------------------------------------------------------------- rules

    /// `GET /rules`
    ///
    /// # Errors
    /// Propagates transport and decode failures.
    pub async fn rules(&self) -> Result<Vec<RuleInfo>> {
        let r: RulesResponse = self
            .send(reqwest::Method::GET, "/rules", "rules", None)
            .await?;
        Ok(r.rules)
    }

    /// `PATCH /rules/disable`
    ///
    /// The body maps rule **index** (as a string) to a disabled flag. Disabling
    /// is in-memory only and is lost on the next configuration reload.
    ///
    /// # Errors
    /// [`Error::Api`] with status 400 for a malformed body.
    pub async fn set_rules_disabled(&self, changes: &BTreeMap<u32, bool>) -> Result<()> {
        if changes.is_empty() {
            return Ok(());
        }
        let body: BTreeMap<String, bool> =
            changes.iter().map(|(k, v)| (k.to_string(), *v)).collect();
        self.send_no_content(
            reqwest::Method::PATCH,
            "/rules/disable",
            Some(serde_json::to_value(body).map_err(|e| Error::serialize("rule disable", e))?),
        )
        .await
    }

    // ---------------------------------------------------------- connections

    /// `GET /connections` — a single snapshot.
    ///
    /// # Errors
    /// Propagates transport and decode failures.
    pub async fn connections(&self) -> Result<ConnectionsResponse> {
        self.send(reqwest::Method::GET, "/connections", "connections", None)
            .await
    }

    /// `DELETE /connections` — close everything.
    ///
    /// Traffic counters are **not** reset by this.
    ///
    /// # Errors
    /// Propagates transport failures.
    pub async fn close_all_connections(&self) -> Result<()> {
        self.send_no_content(reqwest::Method::DELETE, "/connections", None)
            .await
    }

    /// `DELETE /connections/{id}` — close one connection.
    ///
    /// # Errors
    /// Propagates transport failures.
    pub async fn close_connection(&self, id: &str) -> Result<()> {
        let path = format!("/connections/{}", encode_segment(id));
        self.send_no_content(reqwest::Method::DELETE, &path, None)
            .await
    }

    // -------------------------------------------------------------- configs

    /// `GET /configs` — live general settings.
    ///
    /// Note that DNS, proxies, rules and providers are **not** included; this
    /// mirrors the core's split between general settings and the rest.
    ///
    /// # Errors
    /// Propagates transport and decode failures.
    pub async fn configs(&self) -> Result<GeneralConfig> {
        self.send(reqwest::Method::GET, "/configs", "general config", None)
            .await
    }

    /// `PATCH /configs` — change general settings without a restart.
    ///
    /// # Errors
    /// [`Error::Api`] with status 400 for a malformed body.
    pub async fn patch_configs(&self, patch: &ConfigPatch) -> Result<()> {
        if patch.is_empty() {
            return Ok(());
        }
        let body = serde_json::to_value(patch).map_err(|e| Error::serialize("config patch", e))?;
        self.send_no_content(reqwest::Method::PATCH, "/configs", Some(body))
            .await
    }

    /// `PUT /configs` — reload the whole configuration.
    ///
    /// When `payload` is set it is parsed directly and `path` is ignored;
    /// otherwise the file at `path` (absolute, under the working directory) is
    /// read, and an empty path reloads the running file.
    ///
    /// `force` controls whether inbound listeners are torn down and rebuilt.
    /// It defaults to `true` here because that is what "apply everything" means.
    ///
    /// # Errors
    /// [`Error::Api`] with status 400 when the path is unsafe or the YAML is
    /// invalid.
    pub async fn reload_configs(
        &self,
        path: Option<&std::path::Path>,
        payload: Option<&str>,
        force: bool,
    ) -> Result<()> {
        let mut body = serde_json::Map::new();
        if let Some(p) = path {
            let s = p.to_str().ok_or_else(|| {
                Error::invalid("path", format!("{} is not valid UTF-8", p.display()))
            })?;
            body.insert("path".to_owned(), Value::String(s.to_owned()));
        }
        if let Some(pl) = payload {
            body.insert("payload".to_owned(), Value::String(pl.to_owned()));
        }
        let path_str = if force {
            "/configs?force=true"
        } else {
            "/configs"
        };
        self.send_no_content(reqwest::Method::PUT, path_str, Some(Value::Object(body)))
            .await
    }

    /// `POST /configs/geo` — refresh the GeoIP/GeoSite databases.
    ///
    /// # Errors
    /// [`Error::Api`] with status 500 when the download fails.
    pub async fn update_geo(&self) -> Result<()> {
        self.send_no_content(reqwest::Method::POST, "/configs/geo", None)
            .await
    }

    /// `POST /cache/fakeip/flush`
    ///
    /// # Errors
    /// Propagates transport failures.
    pub async fn flush_fakeip(&self) -> Result<()> {
        self.send_no_content(reqwest::Method::POST, "/cache/fakeip/flush", None)
            .await
    }

    /// `POST /cache/dns/flush`
    ///
    /// # Errors
    /// Propagates transport failures.
    pub async fn flush_dns(&self) -> Result<()> {
        self.send_no_content(reqwest::Method::POST, "/cache/dns/flush", None)
            .await
    }

    /// `GET /dns/query`
    ///
    /// Resolves through the core's own resolver, so in fake-IP mode this
    /// returns a synthetic address by design.
    ///
    /// # Errors
    /// [`Error::Api`] with status 500 when the DNS section is disabled.
    pub async fn dns_query(&self, name: &str, record_type: &str) -> Result<Value> {
        let path = format!(
            "/dns/query?name={}&type={}",
            encode_query(name),
            encode_query(record_type)
        );
        self.send(reqwest::Method::GET, &path, "dns query", None)
            .await
    }

    /// `GET /storage/{key}`
    ///
    /// Returns `None` when the key is absent, which the core signals with a
    /// literal `null` body.
    ///
    /// # Errors
    /// Propagates transport failures and decode errors.
    pub async fn storage_get(&self, key: &str) -> Result<Option<Value>> {
        let path = format!("/storage/{}", encode_segment(key));
        let response = self
            .request(reqwest::Method::GET, &path)
            .send()
            .await
            .map_err(|e| Error::ControllerUnreachable {
                endpoint: self.endpoint.describe(),
                source: Box::new(e),
            })?;
        let status = response.status();
        let bytes = response.bytes().await.unwrap_or_default();
        if !status.is_success() {
            return Err(decode_error("GET", &path, status.as_u16(), &bytes));
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|e| Error::Parse {
            kind: "storage value",
            path: path.clone().into(),
            source: Box::new(e),
        })?;
        Ok(if value.is_null() { None } else { Some(value) })
    }

    /// `PUT /storage/{key}`
    ///
    /// # Errors
    /// [`Error::Api`] with status 413 for a payload over 1 MiB.
    pub async fn storage_put(&self, key: &str, value: &Value) -> Result<()> {
        let path = format!("/storage/{}", encode_segment(key));
        self.send_no_content(reqwest::Method::PUT, &path, Some(value.clone()))
            .await
    }

    /// `DELETE /storage/{key}`
    ///
    /// # Errors
    /// Propagates transport failures.
    pub async fn storage_delete(&self, key: &str) -> Result<()> {
        let path = format!("/storage/{}", encode_segment(key));
        self.send_no_content(reqwest::Method::DELETE, &path, None)
            .await
    }

    // ----------------------------------------------------- process control

    /// `POST /restart` — the core re-executes itself.
    ///
    /// The response arrives before the process goes away, so callers must
    /// expect the connection to drop immediately afterwards.
    ///
    /// # Errors
    /// [`Error::Unsupported`] when the build omits the route.
    pub async fn restart(&self) -> Result<()> {
        self.post_status("/restart").await
    }

    /// `POST /upgrade` — replace the core binary.
    ///
    /// `channel` is `release`, `alpha`, or `None` for auto.
    ///
    /// # Errors
    /// [`Error::Api`] with status 500 when already current or the download
    /// fails.
    pub async fn upgrade_core(&self, channel: Option<&str>, force: bool) -> Result<()> {
        let mut path = String::from("/upgrade?");
        if let Some(c) = channel.filter(|c| !c.is_empty()) {
            path.push_str("channel=");
            path.push_str(&encode_query(c));
            path.push('&');
        }
        if force {
            path.push_str("force=true");
        }
        let path = path.trim_end_matches(['?', '&']).to_owned();
        self.post_status(&path).await
    }

    /// `POST /upgrade/geo`
    ///
    /// # Errors
    /// [`Error::Api`] with status 500 when the download fails.
    pub async fn upgrade_geo(&self) -> Result<()> {
        self.send_no_content(reqwest::Method::POST, "/upgrade/geo", None)
            .await
    }

    /// `POST /upgrade/ui` — refresh the bundled dashboard.
    ///
    /// # Errors
    /// [`Error::Api`] with status 500 when `external-ui` is unset or the
    /// download fails.
    pub async fn upgrade_ui(&self) -> Result<()> {
        self.post_status("/upgrade/ui").await
    }

    /// `PUT /debug/gc` — force a garbage collection.
    ///
    /// Only available when the core was started with `log-level: debug`; the
    /// whole `/debug` subtree is absent otherwise.
    ///
    /// # Errors
    /// [`Error::Unsupported`] when the debug router is not mounted.
    pub async fn force_gc(&self) -> Result<()> {
        let response = self
            .request(reqwest::Method::PUT, "/debug/gc")
            .send()
            .await
            .map_err(|e| Error::ControllerUnreachable {
                endpoint: self.endpoint.describe(),
                source: Box::new(e),
            })?;
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        let bytes = response.bytes().await.unwrap_or_default();
        let err = decode_error("PUT", "/debug/gc", status.as_u16(), &bytes);
        if matches!(&err, Error::Api { status: 404, .. }) {
            return Err(Error::Unsupported(
                "the core is not running with log-level: debug, so /debug/gc is not mounted"
                    .to_owned(),
            ));
        }
        Err(err)
    }

    async fn post_status(&self, path: &str) -> Result<()> {
        let r: std::result::Result<StatusOk, Error> =
            self.send(reqwest::Method::POST, path, "status", None).await;
        match r {
            Ok(_) => Ok(()),
            // A build without the route answers plain-text 404.
            Err(Error::Api { status: 404, .. }) => Err(Error::Unsupported(format!(
                "this core build does not expose {path} (embedded builds omit it)"
            ))),
            Err(e) => Err(e),
        }
    }

    // ----------------------------------------------------------- capability

    /// Probe which optional routes this build exposes.
    ///
    /// The core has no capability endpoint, and "not found" is ambiguous: an
    /// unknown *object* and an absent *route* both answer 404. They are
    /// distinguishable by content type — a JSON body means the router matched,
    /// a `text/plain` `404 page not found` means it did not.
    ///
    /// # Errors
    /// Propagates transport failures; a reachable core always yields a report.
    pub async fn probe(&self) -> Result<Capabilities> {
        let version = self.version().await?;
        Ok(Capabilities {
            version: version.trimmed().to_owned(),
            // Bodyless, so the handler rejects it before it can change
            // anything (a real core answers `400 Body invalid`), and a handler
            // that ran is itself the proof that the route exists.
            rules_disable: self
                .route_exists(reqwest::Method::PATCH, "/rules/disable")
                .await,
            configs_write: self.route_exists(reqwest::Method::PATCH, "/configs").await,
            // `PUT` is the only method that reveals whether the `/debug`
            // subtree is mounted: the subtree is absent unless the core was
            // started with `-debug`, and while it is absent *every* method
            // answers a plain-text 404, so a gentler method would report "no
            // debug routes" even on a debug build. The cost is `runtime.GC()`
            // — bounded, idempotent, and the same call `cvt core gc` makes on
            // purpose.
            debug: self.route_exists(reqwest::Method::PUT, "/debug/gc").await,
            // Deliberately *not* the method this family is registered for.
            // `POST /upgrade/geo` calls the geodata updater and starts a
            // multi-megabyte download, so probing a core used to mutate it. A
            // mounted route answers `405 Allow: POST` to any other method and
            // runs no handler at all.
            upgrade: self
                .route_exists(reqwest::Method::PUT, "/upgrade/geo")
                .await,
        })
    }

    /// `true` when the router matched the path, even if the handler errored.
    async fn route_exists(&self, method: reqwest::Method, path: &str) -> bool {
        let Ok(response) = self.request(method, path).send().await else {
            return false;
        };
        // 405 always means the path matched a different method, so the route
        // family exists. 400/401/500 mean a handler ran.
        if response.status().as_u16() != 404 {
            return true;
        }
        is_json_content(response.headers().get(reqwest::header::CONTENT_TYPE))
    }
}

/// Which optional features the running core exposes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    /// Version without the leading `v`.
    pub version: String,
    /// `PATCH /rules/disable` exists (absent in embedded builds).
    pub rules_disable: bool,
    /// `PATCH /configs` exists.
    pub configs_write: bool,
    /// The `/debug` subtree is mounted, i.e. the core logs at debug level.
    pub debug: bool,
    /// `POST /upgrade*` exists.
    pub upgrade: bool,
}

impl Capabilities {
    /// Human-readable summary lines for a status pane.
    #[must_use]
    pub fn summary(&self) -> Vec<(String, String)> {
        let yn = |b: bool| if b { "yes" } else { "no" }.to_owned();
        vec![
            ("core version".to_owned(), self.version.clone()),
            ("rule toggling".to_owned(), yn(self.rules_disable)),
            ("live config writes".to_owned(), yn(self.configs_write)),
            ("debug endpoints".to_owned(), yn(self.debug)),
            ("self-upgrade".to_owned(), yn(self.upgrade)),
        ]
    }
}

// ------------------------------------------------------------------ helpers

fn build_http(endpoint: &Endpoint, timeout: Duration) -> Result<reqwest::Client> {
    #[allow(unused_mut)]
    let mut builder = reqwest::Client::builder()
        .timeout(timeout)
        .user_agent(concat!("clash-verge-tui/", env!("CARGO_PKG_VERSION")))
        // The controller is normally local and may be a bare socket; a proxy
        // would break both cases.
        .no_proxy();

    match &endpoint.transport {
        crate::mihomo::endpoint::Transport::Tcp(_) => {}
        crate::mihomo::endpoint::Transport::Unix(path) => {
            #[cfg(unix)]
            {
                builder = builder.unix_socket(path.clone());
            }
            #[cfg(not(unix))]
            {
                let _ = path;
                return Err(Error::Unsupported(
                    "unix socket controllers are only supported on unix".to_owned(),
                ));
            }
        }
        crate::mihomo::endpoint::Transport::Pipe(name) => {
            #[cfg(target_os = "windows")]
            {
                builder = builder.unix_socket(name.clone());
            }
            #[cfg(not(target_os = "windows"))]
            {
                let _ = name;
                return Err(Error::Unsupported(
                    "named pipe controllers are only supported on Windows".to_owned(),
                ));
            }
        }
    }

    builder.build().map_err(|e| Error::ControllerUnreachable {
        endpoint: endpoint.describe(),
        source: Box::new(e),
    })
}

/// Turn a non-success response into the most specific error available.
fn decode_error(method: &'static str, path: &str, status: u16, body: &[u8]) -> Error {
    let text = String::from_utf8_lossy(body);
    // A JSON envelope means the route handled the request and refused it.
    if let Ok(msg) = serde_json::from_slice::<ApiMessage>(body)
        && !msg.message.is_empty()
    {
        return Error::Api {
            method,
            path: path.to_owned(),
            status,
            body: msg.message,
        };
    }
    if status == 404 && text.contains("404 page not found") {
        return Error::Api {
            method,
            path: path.to_owned(),
            status,
            body: "this route is not present in the running core build".to_owned(),
        };
    }
    Error::Api {
        method,
        path: path.to_owned(),
        status,
        body: text.trim().to_owned(),
    }
}

fn static_method_name(m: &reqwest::Method) -> &'static str {
    match *m {
        reqwest::Method::GET => "GET",
        reqwest::Method::POST => "POST",
        reqwest::Method::PUT => "PUT",
        reqwest::Method::PATCH => "PATCH",
        reqwest::Method::DELETE => "DELETE",
        reqwest::Method::HEAD => "HEAD",
        reqwest::Method::OPTIONS => "OPTIONS",
        _ => "OTHER",
    }
}

fn is_json_content(value: Option<&reqwest::header::HeaderValue>) -> bool {
    value
        .and_then(|v| v.to_str().ok())
        .is_some_and(|s| s.starts_with("application/json"))
}

/// Percent-encode one path segment.
///
/// mihomo unescapes path segments once, so a node named `JP 01` must be sent as
/// `JP%2001` — an unescaped space would be rejected by the router.
#[must_use]
pub fn encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(*byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(*byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// Percent-encode a query-string value.
#[must_use]
pub fn encode_query(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(*byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(*byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// Serialise a value the way the core expects, for tests and callers.
///
/// # Errors
/// [`Error::Serialize`] if the value cannot be encoded.
pub fn to_body<T: Serialize>(value: &T) -> Result<Value> {
    serde_json::to_value(value).map_err(|e| Error::serialize("request body", e))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn encodes_path_segments_the_way_the_core_unescapes_them() {
        assert_eq!(encode_segment("DIRECT"), "DIRECT");
        assert_eq!(encode_segment("JP 01"), "JP%2001");
        assert_eq!(encode_segment("node b/slash"), "node%20b%2Fslash");
        assert_eq!(encode_segment("emoji-🇯🇵"), "emoji-%F0%9F%87%AF%F0%9F%87%B5");
        assert_eq!(encode_segment("a&b=c"), "a%26b%3Dc");
        assert_eq!(encode_segment("50%"), "50%25");
        assert_eq!(
            encode_query("https://gstatic.com/generate_204"),
            "https%3A%2F%2Fgstatic.com%2Fgenerate_204"
        );
        assert_eq!(encode_query("200/204"), "200%2F204");
    }

    #[test]
    fn decodes_the_json_error_envelope() {
        let e = decode_error(
            "GET",
            "/proxies/nope",
            404,
            br#"{"message":"Resource not found"}"#,
        );
        match e {
            Error::Api { status, body, .. } => {
                assert_eq!(status, 404);
                assert_eq!(body, "Resource not found");
            }
            other => panic!("expected an Api error, got {other:?}"),
        }
    }

    #[test]
    fn distinguishes_an_absent_route_from_a_missing_object() {
        let route = decode_error("POST", "/restart", 404, b"404 page not found");
        match route {
            Error::Api { body, .. } => assert!(body.contains("not present"), "{body}"),
            other => panic!("expected an Api error, got {other:?}"),
        }
        // A JSON body, however, means the route exists.
        let object = decode_error(
            "GET",
            "/proxies/x",
            404,
            br#"{"message":"Resource not found"}"#,
        );
        match object {
            Error::Api { body, .. } => assert_eq!(body, "Resource not found"),
            other => panic!("expected an Api error, got {other:?}"),
        }
    }

    #[test]
    fn falls_back_to_the_raw_body_for_a_non_json_error() {
        let e = decode_error("GET", "/x", 500, b"internal explosion");
        match e {
            Error::Api { status, body, .. } => {
                assert_eq!(status, 500);
                assert_eq!(body, "internal explosion");
            }
            other => panic!("expected an Api error, got {other:?}"),
        }
    }

    #[test]
    fn handles_a_405_with_an_empty_body() {
        let e = decode_error("GET", "/providers/rules/x", 405, b"");
        match e {
            Error::Api { status, body, .. } => {
                assert_eq!(status, 405);
                assert!(body.is_empty());
            }
            other => panic!("expected an Api error, got {other:?}"),
        }
    }

    #[test]
    fn builds_urls_for_both_transports() {
        let c = Client::new(Endpoint::tcp("127.0.0.1:9090", Some("s".into()))).unwrap();
        assert_eq!(c.url("/version"), "http://127.0.0.1:9090/version");
        assert_eq!(c.url("version"), "http://127.0.0.1:9090/version");
        assert!(c.endpoint().is_authenticated());

        let u = Client::new(Endpoint::unix("/run/m.sock")).unwrap();
        assert_eq!(u.url("/version"), "http://localhost/version");
        assert!(!u.endpoint().is_authenticated());
    }

    #[test]
    fn rejects_a_bad_endpoint_at_construction() {
        assert!(Client::new(Endpoint::tcp("127.0.0.1", None)).is_err());
        assert!(Client::new(Endpoint::tcp("127.0.0.1:0", None)).is_ok());
    }

    #[tokio::test]
    async fn reports_an_unreachable_controller_clearly() {
        // Port 1 is reserved and nothing listens on it.
        let c = Client::with_timeout(
            Endpoint::tcp("127.0.0.1:1", None),
            Duration::from_millis(500),
        )
        .unwrap();
        let err = c.version().await.unwrap_err();
        assert!(
            matches!(err, Error::ControllerUnreachable { .. }),
            "{err:?}"
        );
        assert!(err.to_string().contains("127.0.0.1:1"), "{err}");
        assert!(!c.is_reachable().await);
    }

    #[test]
    fn serialises_a_patch_without_touching_omitted_fields() {
        let p = ConfigPatch {
            log_level: Some("debug".into()),
            ..ConfigPatch::default()
        };
        assert_eq!(
            to_body(&p).unwrap(),
            serde_json::json!({"log-level": "debug"})
        );
    }
}
