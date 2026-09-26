//! Subscription fetching and updating.
//!
//! Downloading a subscription is easy; downloading one *at the moment the user
//! needs it* is not, because the network path to the panel is often the very
//! thing that is broken. So a refresh walks a short list of routes and stops at
//! the first one that produces a document:
//!
//! 1. **direct** — no proxy at all. The client for this tier calls
//!    `no_proxy()`, so an environment that happens to export `HTTPS_PROXY`
//!    cannot make tier 1 quietly *be* tier 3 and hide the fact that the direct
//!    route is dead.
//! 2. **through the running core's mixed port** — when the direct route is
//!    blocked, a node inside the current subscription usually is not.
//! 3. **the system proxy** — `HTTP_PROXY`/`HTTPS_PROXY`/`ALL_PROXY`, which is
//!    the corporate laptop and the VPN case. This is reqwest's default
//!    behaviour, so the tier deliberately configures *no* proxy of its own.
//!
//! Every tier that was tried is recorded in [`UpdateOutcome::attempts`], the
//! failures included. A fallback that reports only its final verdict leaves the
//! user staring at "update failed" with no way to tell a dead provider from a
//! dead proxy; the list is what lets a panel say which route failed and why.
//!
//! Three smaller decisions carry most of the weight:
//!
//! * The `User-Agent` claims to be mihomo. Many panels gate on it and answer a
//!   browser-shaped client with an HTML landing page instead of a config.
//! * Bodies are normalised by [`decode_body`]. A whole-body base64 blob and a
//!   plain YAML document are both common — sometimes from the same panel,
//!   depending on the `Accept` header it sees.
//! * A body that is not a YAML mapping or list ([`looks_like_config`]) is never
//!   stored, so an error page cannot destroy a working subscription.
//!
//! Updates are sequential: [`SubscriptionFetcher::update_all_due`] walks the due
//! profiles one at a time. Every request arrives from the same address, and
//! firing twenty at once is how an account gets rate-limited for the day.

use std::fmt::Write as _;
use std::path::Path;
use std::time::Duration;

use chrono::Utc;
use futures_util::StreamExt as _;
use reqwest::header::{ACCEPT, ETAG, USER_AGENT};
use serde_json::Value;

use crate::error::{Error, Result};
use crate::profile::item::single_component;
use crate::profile::item::{PrfItem, UserInfo};
use crate::profile::store::ProfileStore;

/// Largest body accepted from a provider, in bytes.
///
/// A subscription is a few hundred kilobytes at worst; anything past this is
/// either a mistake or a hostile response, and either way it must not be
/// buffered into the TUI's memory.
pub const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;

/// Default timeout for one subscription request, covering the body.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// The core release this crate is written against.
///
/// Reported in the `User-Agent`, because panels behave differently depending on
/// which client they think they are talking to.
pub const MIHOMO_COMPAT_VERSION: &str = "1.19.31";

/// `User-Agent` sent with every subscription request.
///
/// `concat!` cannot read a `const`, so the core version is spelled out here too
/// and pinned by a test against [`MIHOMO_COMPAT_VERSION`].
const USER_AGENT_VALUE: &str = concat!(
    "clash-verge-tui/",
    env!("CARGO_PKG_VERSION"),
    " mihomo/1.19.31"
);

/// Header a panel uses to report the account's quota.
const USERINFO_HEADER: &str = "subscription-userinfo";

/// Where the subscription's own page is.
const HOME_HEADER: &str = "profile-web-page-url";

/// How much of a failing response is quoted back in an error message.
const ERROR_BODY_BYTES: usize = 64 * 1024;

/// How many characters of a failing response survive into the message.
const ERROR_BODY_CHARS: usize = 200;

/// Where a successful fetch came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchSource {
    /// Straight to the provider, with no proxy.
    Direct,
    /// Through the mixed port of a running core, e.g. `http://127.0.0.1:7890`.
    ViaClashProxy(String),
    /// Through whatever the environment's proxy variables name.
    ViaSystemProxy,
}

impl FetchSource {
    /// `true` for the tier that bypasses every proxy.
    #[must_use]
    pub fn is_direct(&self) -> bool {
        matches!(self, Self::Direct)
    }
}

impl std::fmt::Display for FetchSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Direct => f.write_str("direct"),
            Self::ViaClashProxy(addr) => write!(f, "clash proxy {addr}"),
            Self::ViaSystemProxy => f.write_str("system proxy"),
        }
    }
}

/// A downloaded subscription document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetched {
    /// The document itself, decoded by [`decode_body`] but otherwise
    /// byte-for-byte as the provider sent it.
    pub body: String,
    /// Quota parsed from the `subscription-userinfo` header; all-zeroes when
    /// the panel did not send one.
    pub user_info: UserInfo,
    /// The subscription's own page, from `profile-web-page-url`.
    ///
    /// Only `http` and `https` are accepted. This is a link the interface will
    /// show and may offer to open, and a panel does not get to hand the user a
    /// `javascript:` or a `file:` one.
    pub home: Option<String>,
    /// A name the panel suggested: `Content-Disposition`'s filename, or the
    /// last segment of the URL when the panel sent none.
    ///
    /// Suggested, not decided: a caller that already has a name keeps it, and
    /// one that does not has something better than a host name.
    pub name: Option<String>,
    /// `Content-Type` of the response, kept for diagnostics.
    pub content_type: Option<String>,
    /// `ETag` of the response.
    ///
    /// Recorded but never sent back as `If-None-Match`: the index has nowhere
    /// to store it for the next run, and comparing the body already detects an
    /// unchanged subscription.
    pub etag: Option<String>,
    /// Bytes received, before any base64 decoding.
    pub bytes: usize,
    /// The route this body arrived on.
    pub source: FetchSource,
}

/// What happened on one tier of a refresh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempt {
    /// The tier that was tried.
    pub source: FetchSource,
    /// Whether it produced a usable document.
    pub ok: bool,
    /// Why it did not, as a single line, when it did not.
    pub error: Option<String>,
}

impl Attempt {
    /// A tier that succeeded.
    fn succeeded(source: FetchSource) -> Self {
        Self {
            source,
            ok: true,
            error: None,
        }
    }

    /// A tier that failed, keeping the reason short enough for a table cell.
    fn failed(source: FetchSource, error: &Error) -> Self {
        Self {
            source,
            ok: false,
            error: Some(error.short()),
        }
    }
}

/// The result of refreshing one profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateOutcome {
    /// Uid of the profile that was refreshed.
    pub uid: String,
    /// Bytes received from the provider.
    pub bytes: usize,
    /// Quota the provider reported on this request.
    pub user_info: UserInfo,
    /// The route that worked.
    pub source: FetchSource,
    /// `true` when the provider sent exactly the document that was already
    /// stored, in which case the file was left alone.
    pub unchanged: bool,
    /// A name the panel suggested on this request, if it sent one.
    ///
    /// Carried out to the caller rather than applied here: whether to adopt a
    /// suggested name is a decision about a profile the user may have named
    /// themselves, and a refresh is not the moment to make it.
    pub suggested_name: Option<String>,
    /// Every tier that was tried, in order, with the failures recorded.
    pub attempts: Vec<Attempt>,
}

impl UpdateOutcome {
    /// Tiers that failed before one succeeded.
    #[must_use]
    pub fn failed_attempts(&self) -> usize {
        self.attempts.iter().filter(|a| !a.ok).count()
    }

    /// One-line summary for a status line or a command's output.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut text = format!("{}: {} bytes via {}", self.uid, self.bytes, self.source);
        if self.unchanged {
            text.push_str(" (unchanged)");
        }
        let failed = self.failed_attempts();
        if failed > 0 {
            let _ = write!(text, " after {failed} failed tier(s)");
        }
        text
    }
}

/// How a tier reaches the network.
///
/// The three cases are genuinely different clients: reqwest reads the
/// environment's proxy variables unless a proxy was configured *and* unless
/// `no_proxy()` was called, so "no explicit proxy" and "no proxy" are not the
/// same thing. Keeping them apart is what makes tier 3 a fallback rather than
/// tier 1 in disguise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route<'a> {
    /// Bypass every proxy.
    Direct,
    /// Send everything through this proxy URL.
    Proxy(&'a str),
    /// Whatever the environment says.
    Environment,
}

impl Route<'_> {
    /// Name used when building the client itself fails.
    fn label(&self) -> &str {
        match self {
            Self::Direct => "direct connection",
            Self::Proxy(addr) => addr,
            Self::Environment => "system proxy",
        }
    }
}

/// Downloads subscriptions, with a fallback for the networks that need one.
#[derive(Debug, Clone)]
pub struct SubscriptionFetcher {
    /// Client for the direct tier, built once because the proxy list of a
    /// `reqwest::Client` is fixed at construction.
    direct: reqwest::Client,
    /// Mixed port of a running core, already normalised; `None` skips tier 2.
    proxy_addr: Option<String>,
    /// Timeout for one request, used when the other tiers build their clients.
    timeout: Duration,
}

impl SubscriptionFetcher {
    /// Build a fetcher.
    ///
    /// `proxy_addr` is the core's mixed port, either `127.0.0.1:7890` or
    /// `http://127.0.0.1:7890`. Pass `None` when no core is running: the tier
    /// that would use it is then skipped instead of failing on every update.
    ///
    /// # Errors
    /// [`Error::Http`] when the HTTP backend cannot be built.
    pub fn new(proxy_addr: Option<String>) -> Result<Self> {
        Ok(Self {
            direct: build_client(Route::Direct, DEFAULT_TIMEOUT)?,
            proxy_addr: proxy_addr.and_then(|addr| normalise_proxy_addr(&addr)),
            timeout: DEFAULT_TIMEOUT,
        })
    }

    /// Override the request timeout.
    ///
    /// The direct client is rebuilt because a `reqwest::Client`'s timeout is
    /// fixed when it is built; the other tiers build their clients per request
    /// and simply pick the new value up.
    ///
    /// # Errors
    /// [`Error::Http`] when the HTTP backend cannot be rebuilt.
    pub fn with_timeout(mut self, timeout: Duration) -> Result<Self> {
        self.direct = build_client(Route::Direct, timeout)?;
        self.timeout = timeout;
        Ok(self)
    }

    /// The configured core mixed-port address, if any.
    #[must_use]
    pub fn proxy_addr(&self) -> Option<&str> {
        self.proxy_addr.as_deref()
    }

    /// The client used for the direct tier.
    #[must_use]
    pub fn client(&self) -> &reqwest::Client {
        &self.direct
    }

    /// The tiers that will be tried, in order.
    ///
    /// Exposed so a panel can show the fallback chain before anything is
    /// fetched, and so the chain is testable without a network.
    #[must_use]
    pub fn fetch_order(&self) -> Vec<FetchSource> {
        let mut order = vec![FetchSource::Direct];
        if let Some(addr) = &self.proxy_addr {
            order.push(FetchSource::ViaClashProxy(addr.clone()));
        }
        order.push(FetchSource::ViaSystemProxy);
        order
    }

    /// Download a subscription over the direct connection.
    ///
    /// The body is decoded but not validated; [`SubscriptionFetcher::update`]
    /// is the operation that decides whether a body is worth storing.
    ///
    /// # Errors
    /// [`Error::InvalidValue`] for a URL that is not `http`/`https`,
    /// [`Error::Http`] for a transport failure, a non-success status or a body
    /// over [`MAX_BODY_BYTES`], and [`Error::InvalidValue`] for a body that
    /// cannot be decoded.
    pub async fn fetch(&self, url: &str) -> Result<Fetched> {
        self.fetch_via(&self.direct, url, FetchSource::Direct).await
    }

    /// Download through a caller-supplied client.
    ///
    /// Suitable for the fallback tiers, which need a client configured for a
    /// proxy. The result is labelled [`FetchSource::Direct`] because only the
    /// caller knows what that client is set up to do; use
    /// [`SubscriptionFetcher::fetch_via`] to record the route as well.
    ///
    /// # Errors
    /// As [`SubscriptionFetcher::fetch`].
    pub async fn fetch_with_client(&self, client: &reqwest::Client, url: &str) -> Result<Fetched> {
        self.fetch_via(client, url, FetchSource::Direct).await
    }

    /// Download through `client`, labelling the result with `source`.
    ///
    /// # Errors
    /// As [`SubscriptionFetcher::fetch`].
    pub async fn fetch_via(
        &self,
        client: &reqwest::Client,
        url: &str,
        source: FetchSource,
    ) -> Result<Fetched> {
        let request = build_request(client, url)?;
        let response = client
            .execute(request)
            .await
            .map_err(|e| Error::http(url, e))?;

        let status = response.status();
        if !status.is_success() {
            // The status is the interesting part; the body is whatever the
            // provider felt like sending, so only a bounded prefix is quoted.
            let detail = read_prefix(response, ERROR_BODY_BYTES).await;
            let reason = format!("HTTP {status}: {detail}");
            return Err(Error::http(url, std::io::Error::other(reason)));
        }

        let content_type = header_string(&response, reqwest::header::CONTENT_TYPE);
        let etag = header_string(&response, ETAG);
        let home = response
            .headers()
            .get(HOME_HEADER)
            .and_then(|value| value.to_str().ok())
            .filter(|value| is_web_url(value))
            .map(str::to_owned);
        let name = response
            .headers()
            .get(reqwest::header::CONTENT_DISPOSITION)
            .and_then(|value| value.to_str().ok())
            .and_then(disposition_filename)
            .or_else(|| name_from_url(url));
        // `HeaderMap` lookups ignore case, so `Subscription-Userinfo` and
        // `subscription-userinfo` are the same header here, as HTTP promises.
        let user_info = response
            .headers()
            .get(USERINFO_HEADER)
            .and_then(|value| value.to_str().ok())
            .map_or_else(UserInfo::default, parse_user_info);

        let raw = read_body(response, url).await?;
        Ok(Fetched {
            bytes: raw.len(),
            body: decode_body(&raw)?,
            user_info,
            home,
            name,
            content_type,
            etag,
            source,
        })
    }

    /// Refresh one profile, trying each tier until one delivers a config.
    ///
    /// The document is written before the index is touched, so a failed write
    /// cannot leave the index claiming an update that never landed. An
    /// unchanged document is not rewritten at all: the core watches the
    /// profiles directory, and reloading the configuration drops every live
    /// connection.
    ///
    /// # Errors
    /// [`Error::ProfileNotFound`] for an unknown uid, [`Error::MissingField`]
    /// when the profile has no subscription URL, [`Error::Http`] when every
    /// tier failed — naming the URL and each tier's reason — and whatever the
    /// document write or index save returned.
    pub async fn update(&self, store: &mut ProfileStore, uid: &str) -> Result<UpdateOutcome> {
        let item = store
            .get(uid)
            .cloned()
            .ok_or_else(|| Error::ProfileNotFound {
                uid: uid.to_owned(),
            })?;
        let url = item
            .url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .ok_or_else(|| Error::MissingField {
                uid: uid.to_owned(),
                field: "url",
            })?
            .to_owned();

        let mut attempts: Vec<Attempt> = Vec::new();
        let mut fetched: Option<Fetched> = None;
        for source in self.fetch_order() {
            match self.attempt_tier(&url, &source).await {
                Ok(body) => {
                    attempts.push(Attempt::succeeded(source));
                    fetched = Some(body);
                    break;
                }
                Err(error) => attempts.push(Attempt::failed(source, &error)),
            }
        }

        let Some(fetched) = fetched else {
            // The reasons travel in the message: by the time this reaches a
            // caller, the only thing left to do is show it to the user.
            return Err(all_tiers_failed(&url, &attempts));
        };
        store_fetched(store, uid, fetched, attempts)
    }

    /// Refresh every remote profile that is due.
    ///
    /// One result per profile rather than a single pass/fail, so a report can
    /// be rendered row by row. Sequential by design: see the module docs.
    pub async fn update_all_due(
        &self,
        store: &mut ProfileStore,
    ) -> Vec<(String, Result<UpdateOutcome>)> {
        let now = Utc::now().timestamp();
        let due: Vec<String> = store
            .items()
            .iter()
            .filter(|item| item.is_remote() && is_due(item, now))
            .map(|item| item.uid.clone())
            .collect();

        let mut results = Vec::with_capacity(due.len());
        for uid in due {
            let outcome = self.update(store, &uid).await;
            results.push((uid, outcome));
        }
        results
    }

    /// Fetch one tier and check that what came back is a config.
    ///
    /// A `200 OK` carrying an HTML captive-portal page is a *failed* tier, not
    /// a success: falling through to the next route is exactly what the user
    /// wants in that situation.
    async fn attempt_tier(&self, url: &str, source: &FetchSource) -> Result<Fetched> {
        let route = match source {
            FetchSource::Direct => Route::Direct,
            FetchSource::ViaClashProxy(addr) => Route::Proxy(addr),
            FetchSource::ViaSystemProxy => Route::Environment,
        };
        let client = build_client(route, self.timeout)?;
        let fetched = self.fetch_via(&client, url, source.clone()).await?;
        if looks_like_config(&fetched.body) {
            return Ok(fetched);
        }
        Err(Error::invalid(
            "body",
            format!(
                "the response is not a mihomo config: {}",
                single_line(&fetched.body, 80)
            ),
        ))
    }
}

/// Apply a successful fetch to the store.
///
/// Split out from [`SubscriptionFetcher::update`] so the part that touches the
/// filesystem can be tested against a real store without a network.
///
/// # Errors
/// [`Error::ProfileNotFound`] when the uid vanished mid-update, and whatever
/// the document write or index save returned.
fn store_fetched(
    store: &mut ProfileStore,
    uid: &str,
    fetched: Fetched,
    attempts: Vec<Attempt>,
) -> Result<UpdateOutcome> {
    let item = store
        .get(uid)
        .cloned()
        .ok_or_else(|| Error::ProfileNotFound {
            uid: uid.to_owned(),
        })?;

    let unchanged = document_matches(store, &item, &fetched.body);
    if !unchanged {
        // Before the index, deliberately: if this fails the index still says
        // the old timestamp, which is true.
        store.write_document(&item, &fetched.body)?;
    }

    if let Some(entry) = store.get_mut(uid) {
        entry.updated = Some(Utc::now().timestamp());
        // Quota headers move on every request even when the document does not,
        // which is most of the reason to refresh an unchanged profile at all.
        entry.extra = fetched.user_info;
        // The page the panel advertises is only known here. Recorded rather
        // than overwritten with `None` when the panel stops sending it: a
        // value that was true once is worth more than a blank.
        if fetched.home.is_some() {
            entry.home.clone_from(&fetched.home);
        }
    }
    store.save()?;

    Ok(UpdateOutcome {
        uid: uid.to_owned(),
        bytes: fetched.bytes,
        user_info: fetched.user_info,
        suggested_name: fetched.name,
        source: fetched.source,
        unchanged,
        attempts,
    })
}

/// `true` when the stored document already holds exactly this body.
///
/// Rewriting an identical document is not harmless: the core watches the
/// profiles directory, so a timer that fires every ten minutes would otherwise
/// tear down every live connection every ten minutes.
fn document_matches(store: &ProfileStore, item: &PrfItem, body: &str) -> bool {
    // An unreadable or absent document simply counts as changed; the write
    // itself is what reports a real filesystem failure.
    store.read_document(item).is_ok_and(|stored| stored == body)
}

/// The error produced when no tier delivered a config.
///
/// Names the URL through [`Error::Http`] and folds every tier's reason into the
/// message, because the caller has nothing left to retry and no other record of
/// what was tried.
fn all_tiers_failed(url: &str, attempts: &[Attempt]) -> Error {
    let mut reason = format!("all {} fetch tier(s) failed", attempts.len());
    for attempt in attempts {
        let detail = attempt.error.as_deref().unwrap_or("no reason recorded");
        let _ = write!(reason, "; {}: {detail}", attempt.source);
    }
    Error::http(url, std::io::Error::other(reason))
}

/// Build a client for one route.
///
/// # Errors
/// [`Error::Http`] when the proxy URL is unusable or the backend cannot be
/// built.
fn build_client(route: Route<'_>, timeout: Duration) -> Result<reqwest::Client> {
    let builder = reqwest::Client::builder()
        .user_agent(USER_AGENT_VALUE)
        .timeout(timeout);

    let builder = match route {
        Route::Direct => builder.no_proxy(),
        Route::Proxy(addr) => {
            let proxy = reqwest::Proxy::all(addr).map_err(|e| Error::http(addr, e))?;
            builder.proxy(proxy)
        }
        // Neither `.proxy()` nor `.no_proxy()`: reqwest then reads the proxy
        // variables from the environment itself, which is the whole tier.
        Route::Environment => builder,
    };

    builder.build().map_err(|e| Error::http(route.label(), e))
}

/// Turn a configured core address into a proxy URL.
///
/// Users type `127.0.0.1:7890` as often as they paste `http://127.0.0.1:7890`,
/// and reqwest needs a scheme. A blank address yields `None`, which skips the
/// tier rather than failing it on every update.
fn normalise_proxy_addr(addr: &str) -> Option<String> {
    let trimmed = addr.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.contains("://") {
        return Some(trimmed.to_owned());
    }
    Some(format!("http://{trimmed}"))
}

/// Parse a URL, rejecting anything that is not an HTTP subscription.
///
/// # Errors
/// [`Error::InvalidValue`] with field `url`.
/// Whether a URL is one the fetcher can use.
///
/// The same check [`validated_url`] makes, for the commands that *record* a URL
/// before fetching it: a profile whose address its own fetcher refuses is a
/// profile that cannot be updated, and the user finds out one step after the
/// step that could have told them.
///
/// # Errors
/// [`Error::InvalidValue`] naming the reason the fetcher would give.
pub fn check_fetchable(url: &str) -> Result<()> {
    validated_url(url).map(|_| ())
}

fn validated_url(url: &str) -> Result<reqwest::Url> {
    let trimmed = url.trim();
    let parsed = reqwest::Url::parse(trimmed)
        .map_err(|e| Error::invalid("url", format!("`{trimmed}` is not a URL: {e}")))?;
    match parsed.scheme() {
        "http" | "https" => Ok(parsed),
        other => Err(Error::invalid(
            "url",
            format!("scheme `{other}` cannot be fetched; use http or https"),
        )),
    }
}

/// Build the request every subscription fetch sends.
///
/// # Errors
/// [`Error::InvalidValue`] for a URL that is not `http`/`https`, or
/// [`Error::Http`] when the request cannot be assembled.
fn build_request(client: &reqwest::Client, url: &str) -> Result<reqwest::Request> {
    // A panel that appends `&token=...` to a URL with no `?` asks for one path
    // segment and gets a 404.
    //
    // The repair used to run only when the URL *failed* to validate, and the
    // shape it exists for — `https://panel/sub&token=abc` — does not fail: `&`
    // is a sub-delim, so it parses and the request went out with the ampersand
    // in it. The repair was dead code for its own case. The shape is detected
    // instead, narrowly: no query yet, and an `=` after the first `&`.
    let repaired = if forgot_its_question_mark(url) {
        repair_url(url).filter(|candidate| validated_url(candidate).is_ok())
    } else {
        None
    };
    let (url, parsed) = match repaired {
        Some(repaired) => {
            let parsed = validated_url(&repaired)
                .unwrap_or_else(|_| unreachable!("filtered by `validated_url` just above"));
            (repaired, parsed)
        }
        None => match validated_url(url) {
            Ok(parsed) => (url.to_owned(), parsed),
            Err(error) => {
                let Some(repaired) = repair_url(url) else {
                    return Err(error);
                };
                let Ok(parsed) = validated_url(&repaired) else {
                    return Err(error);
                };
                (repaired, parsed)
            }
        },
    };
    client
        .get(parsed)
        .header(USER_AGENT, USER_AGENT_VALUE)
        .header(ACCEPT, "*/*")
        .build()
        .map_err(|e| Error::http(url, e))
}

/// Read one response header as a string, ignoring anything non-UTF-8.
fn header_string(
    response: &reqwest::Response,
    name: reqwest::header::HeaderName,
) -> Option<String> {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

/// Read a response body, refusing to buffer more than [`MAX_BODY_BYTES`].
///
/// The streaming form is used rather than `Response::bytes` precisely so that a
/// provider cannot make the process allocate without bound.
///
/// # Errors
/// [`Error::Http`] for a transport failure or an oversized body.
async fn read_body(response: reqwest::Response, url: &str) -> Result<Vec<u8>> {
    if let Some(len) = response
        .content_length()
        .filter(|len| *len > MAX_BODY_BYTES as u64)
    {
        return Err(too_large(url, len));
    }
    let hint = response.content_length().unwrap_or(0).min(1024 * 1024) as usize;
    let mut body: Vec<u8> = Vec::with_capacity(hint);
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| Error::http(url, e))?;
        if body.len() + chunk.len() > MAX_BODY_BYTES {
            return Err(too_large(url, (body.len() + chunk.len()) as u64));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// The error for a body past [`MAX_BODY_BYTES`].
fn too_large(url: &str, len: u64) -> Error {
    Error::http(
        url,
        std::io::Error::other(format!(
            "subscription body is {len} bytes, over the {MAX_BODY_BYTES}-byte limit"
        )),
    )
}

/// Read at most `limit` bytes of a response, for an error message.
///
/// Deliberately infallible: whatever went wrong with the status is the message,
/// and a second failure while explaining the first would only confuse it.
async fn read_prefix(response: reqwest::Response, limit: usize) -> String {
    let mut buffer: Vec<u8> = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(Ok(chunk)) = stream.next().await {
        let take = limit.saturating_sub(buffer.len()).min(chunk.len());
        buffer.extend_from_slice(&chunk[..take]);
        if buffer.len() >= limit {
            break;
        }
    }
    single_line(&String::from_utf8_lossy(&buffer), ERROR_BODY_CHARS)
}

/// Collapse whitespace and truncate, for a message that must stay one line.
fn single_line(text: &str, limit: usize) -> String {
    let mut out = String::with_capacity(limit + 3);
    let mut written = 0usize;
    let mut pending_space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            pending_space = written > 0;
            continue;
        }
        if written >= limit {
            out.push_str("...");
            break;
        }
        if pending_space {
            out.push(' ');
            written += 1;
            pending_space = false;
        }
        out.push(ch);
        written += 1;
    }
    out
}

/// Whether a string is a URL a user can be shown and may open.
#[must_use]
pub fn is_web_url(value: &str) -> bool {
    let trimmed = value.trim();
    let lower = trimmed.to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://"))
        && !trimmed.contains(char::is_whitespace)
}

/// The filename a `Content-Disposition` header suggests, if any.
///
/// Handles the two spellings that exist in the wild: `filename*=UTF-8''<pct>`
/// as RFC 5987 defines it, and the plain quoted `filename="..."`. Neither is
/// validated beyond being non-empty — this is a suggestion, and a panel that
/// sends a strange one simply gets it ignored by whoever called.
#[must_use]
pub fn disposition_filename(header: &str) -> Option<String> {
    let value = header_parameter(header, "filename*=")
        .and_then(|rest| rest.split_once("''").map(|(_, encoded)| encoded.to_owned()))
        .map(|encoded| percent_decode(&encoded))
        .or_else(|| header_parameter(header, "filename=").map(str::to_owned))?;
    let stem = Path::new(value.trim())
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())?;
    let stem = stem.trim().to_owned();
    // A leading dot is not a name: `filename=".yaml"` is a panel sending an
    // extension and nothing else, and `Some(".yaml")` as a display name is
    // worse than the host name the caller already has.
    (!stem.is_empty() && !stem.starts_with('.')).then_some(stem)
}

/// One parameter of a header, quoted or not.
///
/// A quoted value is read to its closing quote: splitting the header on `;`
/// first — which is how this started — cut `filename="My;Airport.yaml"` at the
/// semicolon *inside* the quotes, and that is a legal value. An unquoted one
/// runs to the next `;`, which is the shape RFC 5987's extended form actually
/// uses, so both spellings have to be read this way.
fn header_parameter<'a>(header: &'a str, key: &str) -> Option<&'a str> {
    let start = header.find(key)? + key.len();
    let rest = header[start..].trim_start();
    match rest.strip_prefix('"') {
        Some(quoted) => quoted.find('"').map(|end| &quoted[..end]),
        None => Some(rest.split(';').next().unwrap_or(rest).trim()),
    }
}

/// The last meaningful segment of a URL, as a name.
fn name_from_url(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let after_scheme = path.split_once("://").map_or(path, |(_, rest)| rest);
    // Everything before the first `/` is the authority, and a name can only
    // come from what follows it — so a URL with no path has none to offer. The
    // earlier version took the last `/`-separated piece of the whole URL, which
    // for `https://panel.example.com` is the host, and offering the host back
    // as a *suggestion* is worse than offering nothing: the caller already
    // falls back to it.
    let (_, tail) = after_scheme.split_once('/')?;
    // Decoded *before* the split. Splitting first and decoding the chosen
    // segment afterwards let `%2e%2e%2f%2e%2e%2fetc%2fpasswd` through as
    // `../../etc/passwd`: the separators were not separators until they were
    // decoded, and by then the segment had already been picked.
    let decoded = percent_decode(tail);
    let segment = decoded
        .trim_end_matches('/')
        .rsplit('/')
        .find(|part| !part.is_empty())?;
    // The same guard the file names go through. Nothing uses a profile name as
    // a path today, and that is not a reason to hand out one that could be.
    single_component(segment)
}

/// `true` when a URL looks like a query string whose `?` was forgotten.
///
/// No query or fragment yet, and an `=` after the first `&`. An ampersand in a
/// path is legal on its own, which is why the second half is there.
fn forgot_its_question_mark(url: &str) -> bool {
    if url.contains(['?', '#']) {
        return false;
    }
    url.split_once('&')
        .is_some_and(|(_, rest)| rest.contains('='))
}

/// Repair the `path&a=b` spelling of a query string.
///
/// A panel that builds its subscription URL by appending `&token=...` to a URL
/// with no `?` produces a query string nobody sent: the whole thing is one path
/// segment, and the server answers 404. It is a common enough mistake — and the
/// reference project carries the same repair — that guessing the intended URL
/// is friendlier than reporting the 404 the panel earned.
#[must_use]
pub fn repair_url(url: &str) -> Option<String> {
    let (path, rest) = url.split_once('&')?;
    if rest.is_empty() || url.contains('?') || url.contains('#') {
        return None;
    }
    Some(format!("{path}?{rest}"))
}

/// Decode `%xx` escapes, leaving anything malformed as it stands.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parse the `subscription-userinfo` header.
///
/// The format panels emit is
/// `upload=1234; download=5678; total=100000000; expire=1800000000`, in whatever
/// spacing and capitalisation they feel like. Keys are matched
/// case-insensitively, unknown keys are ignored, and a missing or malformed
/// value defaults to zero: a quota display is not worth failing an update over,
/// and garbage here must never panic a UI.
#[must_use]
pub fn parse_user_info(header: &str) -> UserInfo {
    let mut info = UserInfo::default();
    for field in header.split(';') {
        let Some((key, value)) = field.split_once('=') else {
            continue;
        };
        let Some(value) = parse_u64(value) else {
            continue;
        };
        match key.trim().to_ascii_lowercase().as_str() {
            "upload" => info.upload = value,
            "download" => info.download = value,
            "total" => info.total = value,
            "expire" => info.expire = value,
            _ => {}
        }
    }
    info
}

/// Parse a header value, tolerating quotes and surrounding space.
fn parse_u64(raw: &str) -> Option<u64> {
    raw.trim()
        .trim_matches(|c| c == '"' || c == '\'')
        .parse::<u64>()
        .ok()
}

/// Turn a downloaded response into the document it carries.
///
/// Three shapes are in circulation and all three arrive with a `200 OK`:
///
/// * plain YAML (and JSON, which is a subset of YAML),
/// * the whole document base64-encoded, which is what a panel does when it
///   thinks it is talking to a browser or another client,
/// * an error page from a captive portal or a dead CDN.
///
/// The decode is conservative on purpose. A base64 body is itself valid UTF-8,
/// so it is only accepted when the *decoded* text parses as a configuration;
/// otherwise the original text is handed back untouched for the caller to
/// report, rather than being replaced by decoded noise. Bytes that are not
/// UTF-8 at all can only be base64, so there the result is returned as soon as
/// it is a valid string.
///
/// A byte-order mark is stripped: a UTF-8 BOM in front of a YAML document is a
/// parse error in most readers, and some panels emit one.
///
/// # Errors
/// [`Error::InvalidValue`] with field `body` for an empty response, or for
/// bytes that are neither UTF-8 text nor base64.
pub fn decode_body(raw: &[u8]) -> Result<String> {
    let raw = strip_bom(raw);
    if raw.iter().all(u8::is_ascii_whitespace) {
        return Err(Error::invalid(
            "body",
            "the provider returned an empty body",
        ));
    }

    let Ok(text) = std::str::from_utf8(raw) else {
        return decode_encoded_body(raw);
    };
    if looks_like_config(text) {
        return Ok(text.to_owned());
    }
    if let Some(decoded) = base64_text(raw).filter(|decoded| looks_like_config(decoded)) {
        return Ok(decoded);
    }
    Ok(text.to_owned())
}

/// Decode bytes that are not UTF-8, which leaves base64 as the only option.
fn decode_encoded_body(raw: &[u8]) -> Result<String> {
    let decoded = base64_decode(raw).ok_or_else(|| {
        Error::invalid(
            "body",
            format!(
                "the subscription body is neither UTF-8 text nor base64 ({} bytes)",
                raw.len()
            ),
        )
    })?;
    let text = String::from_utf8(decoded)
        .map_err(|e| Error::invalid("body", format!("the base64-decoded body is not text: {e}")))?;
    Ok(strip_bom_str(&text).to_owned())
}

/// Decode base64 into a string, when it produces one.
fn base64_text(raw: &[u8]) -> Option<String> {
    let decoded = base64_decode(raw)?;
    String::from_utf8(decoded)
        .ok()
        .map(|text| strip_bom_str(&text).to_owned())
}

/// `true` when `body` parses as a YAML mapping or list.
///
/// A scalar — which is what an HTML error page, a bare token or a base64 blob
/// reduces to — is not a configuration. This is the check that keeps a provider
/// having a bad day from overwriting a subscription that still works.
#[must_use]
pub fn looks_like_config(body: &str) -> bool {
    matches!(
        serde_norway::from_str::<Value>(body),
        Ok(Value::Object(_) | Value::Array(_))
    )
}

/// Strip a UTF-8 byte-order mark.
fn strip_bom(raw: &[u8]) -> &[u8] {
    raw.strip_prefix(b"\xEF\xBB\xBF".as_slice()).unwrap_or(raw)
}

/// Strip a UTF-8 byte-order mark from text.
fn strip_bom_str(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

/// Decode standard or URL-safe base64, with or without padding.
///
/// Written out rather than pulled in as a dependency: it is forty lines, it
/// removes an entire crate from the supply chain, and it lets the decoder be
/// strict about the encodings a panel can plausibly send — whitespace ignored,
/// both alphabets accepted, anything else rejected.
fn base64_decode(input: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let mut accumulator: u32 = 0;
    let mut bits: u32 = 0;
    let mut padding = 0usize;
    let mut data = 0usize;

    for byte in input {
        if byte.is_ascii_whitespace() {
            continue;
        }
        if *byte == b'=' {
            padding += 1;
            continue;
        }
        // Padding is only meaningful at the end, so data after it is malformed.
        if padding > 0 {
            return None;
        }
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        };
        accumulator = (accumulator << 6) | u32::from(value);
        bits += 6;
        data += 1;
        if bits >= 8 {
            bits -= 8;
            out.push(((accumulator >> bits) & 0xFF) as u8);
        }
    }

    // A base64 group is 4 characters; one left over means a truncated body, and
    // leftover bits that are not zero mean the text was never base64.
    if data == 0 || data % 4 == 1 || padding > 2 {
        return None;
    }
    if bits > 0 && (accumulator & ((1 << bits) - 1)) != 0 {
        return None;
    }
    Some(out)
}

/// `true` when the profile is due for a refresh.
///
/// Due means `now - updated >= interval`, where the interval comes from
/// [`PrfOption::effective_interval`] and therefore already requires both
/// `allow_auto_update: true` and a positive `update_interval`. A profile that
/// has an interval but has never been fetched is due: the interval is exactly
/// what schedules the first fetch.
///
/// [`PrfOption::effective_interval`]: crate::profile::item::PrfOption::effective_interval
#[must_use]
pub fn is_due(item: &PrfItem, now_unix: i64) -> bool {
    let Some(minutes) = item.option.effective_interval() else {
        return false;
    };
    let Some(updated) = item.updated else {
        return true;
    };
    // Saturating arithmetic: an absurd interval, or a clock that jumped
    // forwards, must not make the comparison wrap into "due now".
    let seconds = i64::try_from(minutes)
        .unwrap_or(i64::MAX)
        .saturating_mul(60);
    now_unix >= updated.saturating_add(seconds)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::paths::AppPaths;
    use crate::profile::item::PrfOption;
    use crate::profile::store::document_path;
    use tempfile::TempDir;

    fn item_with_triggers(
        interval: Option<u64>,
        allow: Option<bool>,
        updated: Option<i64>,
    ) -> PrfItem {
        let mut item = PrfItem::remote("R1", "Airport", "https://example.com/sub");
        item.option = PrfOption {
            update_interval: interval,
            allow_auto_update: allow,
            ..PrfOption::default()
        };
        item.updated = updated;
        item
    }

    fn store_with_remote(uid: &str, url: &str) -> (TempDir, ProfileStore) {
        let dir = TempDir::new().unwrap();
        let paths = AppPaths::new(dir.path());
        paths.ensure_dirs().unwrap();
        let mut store = ProfileStore::load(&paths).unwrap();
        store.add(PrfItem::remote(uid, "Airport", url));
        (dir, store)
    }

    #[test]
    fn a_panel_suggested_name_is_read_from_whichever_spelling_it_used() {
        // RFC 5987.
        assert_eq!(
            disposition_filename("attachment; filename*=UTF-8''My%20Airport.yaml").as_deref(),
            Some("My Airport")
        );
        // The plain, older spelling, with and without a quoted value.
        assert_eq!(
            disposition_filename("attachment; filename=\"airport.yaml\"").as_deref(),
            Some("airport")
        );
        assert_eq!(
            disposition_filename("attachment; filename=airport.yaml").as_deref(),
            Some("airport")
        );
        // Nothing usable is `None` rather than an empty name.
        assert_eq!(disposition_filename("inline"), None);
        assert_eq!(disposition_filename("attachment; filename=\"\""), None);
        assert_eq!(disposition_filename("attachment; filename=\".yaml\""), None);
    }

    #[test]
    fn a_url_gives_a_name_only_when_it_has_one_to_give() {
        assert_eq!(
            name_from_url("https://panel.example.com/sub/MyAirport").as_deref(),
            Some("MyAirport")
        );
        assert_eq!(
            name_from_url("https://panel.example.com/sub/My%20Airport?token=x").as_deref(),
            Some("My Airport")
        );
        // The host is not a name: `default_name` already covers that, and a
        // suggestion of `panel.example.com` would be worse than nothing.
        assert_eq!(name_from_url("https://panel.example.com"), None);
        assert_eq!(name_from_url("https://panel.example.com/"), None);
    }

    #[test]
    fn a_query_string_without_its_question_mark_is_repaired() {
        assert_eq!(
            repair_url("https://panel.example.com/api/v1/client/subscribe&token=abc").as_deref(),
            Some("https://panel.example.com/api/v1/client/subscribe?token=abc")
        );
        // Only the broken shape, and only once: a `?` already there means the
        // URL is somebody's deliberate construction, not a slip.
        assert_eq!(repair_url("https://x.example/sub?token=abc"), None);
        assert_eq!(repair_url("https://x.example/sub&"), None);
        assert_eq!(repair_url("https://x.example/sub"), None);
    }

    #[test]
    fn only_a_web_url_is_offered_as_a_home_page() {
        assert!(is_web_url("https://panel.example.com/dashboard"));
        assert!(is_web_url("http://panel.example.com"));
        // The interface shows this link and may offer to open it.
        assert!(!is_web_url("javascript:alert(1)"));
        assert!(!is_web_url("file:///etc/passwd"));
        assert!(!is_web_url("data:text/html,x"));
        assert!(!is_web_url("https://example.com/a b"));
    }

    fn fetched(body: &str) -> Fetched {
        Fetched {
            home: None,
            name: None,
            body: body.to_owned(),
            user_info: UserInfo {
                upload: 1,
                download: 2,
                total: 3,
                expire: 4,
            },
            content_type: Some("text/yaml".to_owned()),
            etag: Some("\"abc\"".to_owned()),
            bytes: body.len(),
            source: FetchSource::Direct,
        }
    }

    // ------------------------------------------------------------ user info

    #[test]
    fn parses_the_header_a_panel_actually_sends() {
        let info =
            parse_user_info("upload=1234; download=5678; total=100000000; expire=1800000000");
        assert_eq!(info.upload, 1234);
        assert_eq!(info.download, 5678);
        assert_eq!(info.total, 100_000_000);
        assert_eq!(info.expire, 1_800_000_000);
        assert_eq!(info.used(), 6_912);
    }

    #[test]
    fn user_info_keys_are_case_insensitive_and_spacing_is_free() {
        let info = parse_user_info("UPLOAD = 1 ;Download=2;  TOTAL=3;expire=4");
        assert_eq!(info.upload, 1);
        assert_eq!(info.download, 2);
        assert_eq!(info.total, 3);
        assert_eq!(info.expire, 4);
        assert_eq!(
            parse_user_info("upload=1; plan=pro").total,
            0,
            "an unknown key is ignored, not fatal"
        );
    }

    #[test]
    fn the_userinfo_header_name_is_matched_case_insensitively() {
        // The lookup in `fetch_via` leans on `HeaderMap` normalising header
        // names, and panels send every capitalisation of this one.
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::HeaderName::from_static("subscription-userinfo"),
            reqwest::header::HeaderValue::from_static("upload=7; total=9"),
        );
        let info = headers
            .get(USERINFO_HEADER)
            .or_else(|| headers.get("Subscription-Userinfo"))
            .and_then(|value| value.to_str().ok())
            .map_or_else(UserInfo::default, parse_user_info);
        assert_eq!(info.upload, 7);
        assert_eq!(info.total, 9);
    }

    #[test]
    fn user_info_ignores_unknown_and_malformed_fields_without_panicking() {
        let info = parse_user_info("upload=10; plan=pro; total=; expire=soon; =42; traffic=1.5");
        assert_eq!(info.upload, 10);
        assert_eq!(info.total, 0, "an empty value is absent, not zero-valued");
        assert_eq!(info.expire, 0);
        assert!(parse_user_info("").is_empty());
        assert!(parse_user_info(";;;;").is_empty());
        assert!(parse_user_info("total").is_empty());
        assert!(
            parse_user_info("expire=-5").is_empty(),
            "a negative quota is garbage"
        );
        assert!(
            parse_user_info("total=99999999999999999999999").is_empty(),
            "a value past u64 is garbage rather than a wrapped number"
        );
    }

    #[test]
    fn quoted_values_and_repeats_are_handled() {
        let info = parse_user_info(r#"upload="10"; download='20'; download=30"#);
        assert_eq!(info.upload, 10);
        assert_eq!(info.download, 30, "the last value for a key wins");
    }

    // ---------------------------------------------------------- body decode

    #[test]
    fn a_plain_yaml_document_is_passed_through_untouched() {
        let body = "mixed-port: 7890\nmode: rule\n";
        assert_eq!(decode_body(body.as_bytes()).unwrap(), body);
        assert_eq!(
            decode_body(b"- DIRECT\n- REJECT\n").unwrap(),
            "- DIRECT\n- REJECT\n",
            "a list is a config too"
        );
    }

    #[test]
    fn a_json_document_is_passed_through_untouched() {
        let body = r#"{"port":7890,"mode":"rule"}"#;
        assert_eq!(decode_body(body.as_bytes()).unwrap(), body);
        assert!(looks_like_config(body));
    }

    #[test]
    fn a_base64_document_is_decoded() {
        let plain = "mixed-port: 7890\nmode: rule\n";
        let encoded = "bWl4ZWQtcG9ydDogNzg5MAptb2RlOiBydWxlCg==";
        assert_eq!(decode_body(encoded.as_bytes()).unwrap(), plain);

        // Same document without padding, which several panels emit.
        let unpadded = encoded.trim_end_matches('=');
        assert_eq!(decode_body(unpadded.as_bytes()).unwrap(), plain);

        // Wrapped across lines, which the other half emit.
        let wrapped = "bWl4ZWQtcG9ydDogNzg5\nMAptb2RlOiBydWxlCg==";
        assert_eq!(decode_body(wrapped.as_bytes()).unwrap(), plain);

        // Base64 that is only valid once it is read as bytes rather than text.
        let bytes = base64_decode(encoded.as_bytes()).unwrap();
        assert_eq!(decode_body(&bytes).unwrap(), plain);
    }

    #[test]
    fn a_base64_json_document_is_decoded_too() {
        let plain = r#"{"port":7890,"mode":"rule"}"#;
        let encoded = "eyJwb3J0Ijo3ODkwLCJtb2RlIjoicnVsZSJ9";
        assert_eq!(decode_body(encoded.as_bytes()).unwrap(), plain);
    }

    #[test]
    fn an_error_page_is_handed_back_rather_than_decoded_into_noise() {
        let html = "<!doctype html>\n<html><body>404 not found</body></html>\n";
        let decoded = decode_body(html.as_bytes()).unwrap();
        assert_eq!(decoded, html, "the caller needs to see what was sent");
        assert!(!looks_like_config(&decoded));

        // Text that happens to be base64-shaped but is not a config must not be
        // replaced by whatever it decodes to.
        assert_eq!(decode_body(b"abcdefgh").unwrap(), "abcdefgh");
    }

    #[test]
    fn an_empty_body_is_an_error() {
        assert!(decode_body(b"").is_err());
        assert!(decode_body(b"   \n\t").is_err());
    }

    #[test]
    fn bytes_that_are_neither_text_nor_base64_are_an_error() {
        let err = decode_body(&[0xFF, 0xFE, 0x00, 0x01, 0x80]).unwrap_err();
        assert!(
            matches!(err, Error::InvalidValue { field: "body", .. }),
            "{err:?}"
        );
    }

    #[test]
    fn a_byte_order_mark_is_stripped_before_parsing() {
        let with_bom = b"\xEF\xBB\xBFmixed-port: 7890\n";
        assert_eq!(decode_body(with_bom).unwrap(), "mixed-port: 7890\n");

        // An encoded body can carry one as well, in which case the BOM only
        // appears after the decode.
        let mut encoded = b"\xEF\xBB\xBF".to_vec();
        encoded.extend_from_slice(b"bWl4ZWQtcG9ydDogNzg5MAptb2RlOiBydWxlCg==");
        assert_eq!(
            decode_body(&encoded).unwrap(),
            "mixed-port: 7890\nmode: rule\n"
        );
    }

    #[test]
    fn the_base64_decoder_accepts_both_alphabets_and_no_padding() {
        assert_eq!(base64_decode(b"aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64_decode(b"aGVsbG8").unwrap(), b"hello");
        assert_eq!(base64_decode(b"aGVs\nbG8=").unwrap(), b"hello");
        assert_eq!(base64_decode(b"-_-_").unwrap(), vec![0xFB, 0xFF, 0xBF]);
        assert_eq!(base64_decode(b"+/+/").unwrap(), vec![0xFB, 0xFF, 0xBF]);
        // Trailing bits that are not zero mean this was never base64.
        assert_eq!(base64_decode(b"QQ").unwrap(), b"A");
        assert!(base64_decode(b"QR").is_none());
        // A truncated group, stray characters, padding in the middle, no data.
        assert!(
            base64_decode(b"aGVsbG8xy").is_none(),
            "nine characters is not a group"
        );
        assert!(base64_decode(b"aGVs bG8*").is_none());
        assert!(base64_decode(b"aGVs=G8").is_none());
        assert!(base64_decode(b"====").is_none());
        assert!(base64_decode(b"").is_none());
    }

    #[test]
    fn looks_like_config_rejects_everything_that_is_not_a_document() {
        assert!(looks_like_config("mixed-port: 7890"));
        assert!(looks_like_config("[]"));
        assert!(looks_like_config("proxies:\n  - {name: a}"));
        assert!(!looks_like_config(""));
        assert!(!looks_like_config("   "));
        assert!(!looks_like_config("bWl4ZWQtcG9ydDogNzg5MA=="));
        assert!(!looks_like_config("<!doctype html><html></html>"));
        assert!(!looks_like_config("404 not found"));
    }

    // --------------------------------------------------------------- is_due

    #[test]
    fn a_never_fetched_profile_with_an_interval_is_due() {
        assert!(is_due(
            &item_with_triggers(Some(60), Some(true), None),
            1_700_000_000
        ));
    }

    #[test]
    fn a_profile_without_an_effective_interval_is_never_due() {
        let now = 1_700_000_000;
        assert!(!is_due(&item_with_triggers(None, Some(true), None), now));
        assert!(
            !is_due(&item_with_triggers(Some(60), None, None), now),
            "auto-update must be allowed explicitly"
        );
        assert!(!is_due(
            &item_with_triggers(Some(60), Some(false), None),
            now
        ));
        assert!(
            !is_due(&item_with_triggers(Some(0), Some(true), None), now),
            "zero minutes means disabled"
        );
    }

    #[test]
    fn a_profile_becomes_due_when_the_interval_has_elapsed() {
        let now = 1_700_000_000;
        let minute = 60;
        assert!(!is_due(
            &item_with_triggers(Some(5), Some(true), Some(now - 4 * minute)),
            now
        ));
        assert!(
            is_due(
                &item_with_triggers(Some(5), Some(true), Some(now - 5 * minute)),
                now
            ),
            "exactly due counts as due"
        );
        assert!(is_due(
            &item_with_triggers(Some(5), Some(true), Some(now - 6 * minute)),
            now
        ));
        assert!(
            !is_due(
                &item_with_triggers(Some(5), Some(true), Some(now + 3600)),
                now
            ),
            "a clock that jumped backwards must not make everything due"
        );
    }

    #[test]
    fn an_absurd_interval_does_not_overflow() {
        assert!(!is_due(
            &item_with_triggers(Some(u64::MAX), Some(true), Some(0)),
            1_700_000_000
        ));
    }

    // -------------------------------------------------------------- requests

    #[test]
    fn the_user_agent_names_the_project_and_the_mihomo_version() {
        assert_eq!(
            USER_AGENT_VALUE,
            format!(
                "clash-verge-tui/{} mihomo/{}",
                env!("CARGO_PKG_VERSION"),
                MIHOMO_COMPAT_VERSION
            ),
            "the literal in `concat!` must stay in step with the const"
        );
    }

    #[tokio::test]
    async fn the_request_carries_the_headers_panels_gate_on() {
        let client = build_client(Route::Direct, DEFAULT_TIMEOUT).unwrap();
        let request = build_request(&client, "https://example.com/sub?token=abc").unwrap();
        assert_eq!(request.method(), reqwest::Method::GET);
        assert_eq!(
            request.headers().get(USER_AGENT).unwrap().to_str().unwrap(),
            USER_AGENT_VALUE
        );
        assert_eq!(
            request.headers().get(ACCEPT).unwrap().to_str().unwrap(),
            "*/*"
        );
        assert_eq!(
            request.url().as_str(),
            "https://example.com/sub?token=abc",
            "the query string must survive untouched"
        );
    }

    #[test]
    fn only_http_and_https_urls_can_be_fetched() {
        for bad in [
            "file:///etc/passwd",
            "ftp://example.com/sub",
            "",
            "   ",
            "not a url",
            "https://",
        ] {
            let err = validated_url(bad).unwrap_err();
            assert!(
                matches!(err, Error::InvalidValue { field: "url", .. }),
                "{bad}: {err:?}"
            );
        }
        assert!(validated_url("https://example.com/sub?token=a").is_ok());
        assert!(validated_url("http://127.0.0.1:8080/sub").is_ok());
    }

    #[tokio::test]
    async fn a_client_is_built_for_each_route() {
        assert!(build_client(Route::Direct, DEFAULT_TIMEOUT).is_ok());
        assert!(build_client(Route::Environment, DEFAULT_TIMEOUT).is_ok());
        assert!(build_client(Route::Proxy("http://127.0.0.1:7890"), DEFAULT_TIMEOUT).is_ok());
        let err = build_client(Route::Proxy("not a proxy"), DEFAULT_TIMEOUT).unwrap_err();
        assert!(matches!(err, Error::Http { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn the_fallback_order_skips_the_core_tier_without_an_address() {
        let without = SubscriptionFetcher::new(None).unwrap();
        assert_eq!(
            without.fetch_order(),
            vec![FetchSource::Direct, FetchSource::ViaSystemProxy]
        );

        let with = SubscriptionFetcher::new(Some("127.0.0.1:7890".to_owned())).unwrap();
        assert_eq!(with.proxy_addr(), Some("http://127.0.0.1:7890"));
        assert_eq!(
            with.fetch_order(),
            vec![
                FetchSource::Direct,
                FetchSource::ViaClashProxy("http://127.0.0.1:7890".to_owned()),
                FetchSource::ViaSystemProxy,
            ]
        );

        let blank = SubscriptionFetcher::new(Some("   ".to_owned())).unwrap();
        assert!(
            blank.proxy_addr().is_none(),
            "a blank address must not become a tier that always fails"
        );
        assert_eq!(
            SubscriptionFetcher::new(Some("http://127.0.0.1:7890".to_owned()))
                .unwrap()
                .proxy_addr(),
            Some("http://127.0.0.1:7890"),
            "an address that already has a scheme is left alone"
        );
    }

    #[test]
    fn proxy_addresses_are_normalised_and_blank_ones_dropped() {
        assert_eq!(
            normalise_proxy_addr(" 127.0.0.1:7890 "),
            Some("http://127.0.0.1:7890".to_owned())
        );
        assert_eq!(
            normalise_proxy_addr("socks5://127.0.0.1:7891"),
            Some("socks5://127.0.0.1:7891".to_owned())
        );
        assert_eq!(normalise_proxy_addr(""), None);
        assert_eq!(normalise_proxy_addr("  "), None);
    }

    #[test]
    fn sources_render_for_a_report() {
        assert_eq!(FetchSource::Direct.to_string(), "direct");
        assert_eq!(
            FetchSource::ViaClashProxy("http://127.0.0.1:7890".to_owned()).to_string(),
            "clash proxy http://127.0.0.1:7890"
        );
        assert_eq!(FetchSource::ViaSystemProxy.to_string(), "system proxy");
        assert!(FetchSource::Direct.is_direct());
        assert!(!FetchSource::ViaSystemProxy.is_direct());
    }

    #[test]
    fn a_message_is_one_line_and_bounded() {
        assert_eq!(single_line("  a \n b\t\tc ", 40), "a b c");
        let long = single_line(&"x".repeat(500), 10);
        assert_eq!(long, "xxxxxxxxxx...");
        assert_eq!(single_line("", 10), "");
    }

    // ---------------------------------------------------------------- update

    #[tokio::test]
    async fn updating_an_unknown_profile_fails_before_any_request() {
        let (_dir, mut store) = store_with_remote("R1", "https://example.com/sub");
        let fetcher = SubscriptionFetcher::new(None).unwrap();
        let err = fetcher.update(&mut store, "ghost").await.unwrap_err();
        assert!(matches!(err, Error::ProfileNotFound { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn a_profile_without_a_url_reports_the_missing_field() {
        let (_dir, mut store) = store_with_remote("R1", "https://example.com/sub");
        store.get_mut("R1").unwrap().url = Some("   ".to_owned());
        let fetcher = SubscriptionFetcher::new(None).unwrap();
        let err = fetcher.update(&mut store, "R1").await.unwrap_err();
        assert!(
            matches!(err, Error::MissingField { field: "url", .. }),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn a_file_url_fails_every_tier_and_is_reported_tier_by_tier() {
        let (_dir, mut store) = store_with_remote("R1", "file:///etc/passwd");
        let fetcher = SubscriptionFetcher::new(Some("127.0.0.1:7890".to_owned())).unwrap();
        let err = fetcher.update(&mut store, "R1").await.unwrap_err();
        let message = err.to_string();
        assert!(message.contains("file:///etc/passwd"), "{message}");
        assert!(message.contains("3 fetch tier(s) failed"), "{message}");
        for tier in [
            "direct",
            "clash proxy http://127.0.0.1:7890",
            "system proxy",
        ] {
            assert!(message.contains(tier), "{tier} missing from: {message}");
        }
        assert!(message.contains("scheme"), "{message}");

        // Nothing may be written for a failed update.
        assert!(store.get("R1").unwrap().updated.is_none());
        assert!(!document_path(store.paths(), store.get("R1").unwrap()).exists());
    }

    #[test]
    fn storing_a_fetch_writes_the_document_and_persists_the_index() {
        let (_dir, mut store) = store_with_remote("R1", "https://example.com/sub");
        let outcome = store_fetched(
            &mut store,
            "R1",
            fetched("mixed-port: 7890\n"),
            vec![Attempt::succeeded(FetchSource::Direct)],
        )
        .unwrap();

        assert!(!outcome.unchanged);
        assert_eq!(outcome.source, FetchSource::Direct);
        assert_eq!(outcome.attempts.len(), 1);
        assert_eq!(outcome.bytes, "mixed-port: 7890\n".len());

        let item = store.get("R1").unwrap();
        assert_eq!(
            store.read_document(item).unwrap(),
            "mixed-port: 7890\n",
            "the document must be the provider's bytes"
        );
        let updated = item.updated.unwrap();
        assert_eq!(item.extra.total, 3, "the quota is recorded too");

        let reloaded = ProfileStore::load(store.paths()).unwrap();
        assert_eq!(reloaded.get("R1").unwrap().updated, Some(updated));
        assert_eq!(reloaded.get("R1").unwrap().extra.expire, 4);
    }

    #[test]
    fn an_unchanged_subscription_does_not_churn_the_document() {
        let (_dir, mut store) = store_with_remote("R1", "https://example.com/sub");
        store_fetched(&mut store, "R1", fetched("mode: rule\n"), Vec::new()).unwrap();
        let path = document_path(store.paths(), store.get("R1").unwrap());
        let before = std::fs::metadata(&path).unwrap().modified().unwrap();

        // A rewrite renames a fresh file into place, which always moves the
        // modification time even when the bytes are identical.
        std::thread::sleep(Duration::from_millis(20));
        let outcome = store_fetched(&mut store, "R1", fetched("mode: rule\n"), Vec::new()).unwrap();
        assert!(outcome.unchanged);
        let after = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(before, after, "an unchanged body must not touch the file");

        // A quota change alone still counts as a successful refresh.
        let mut moved = fetched("mode: rule\n");
        moved.user_info.total = 999;
        let outcome = store_fetched(&mut store, "R1", moved, Vec::new()).unwrap();
        assert!(outcome.unchanged);
        assert_eq!(store.get("R1").unwrap().extra.total, 999);
    }

    #[test]
    fn a_changed_subscription_replaces_the_document() {
        let (_dir, mut store) = store_with_remote("R1", "https://example.com/sub");
        store_fetched(&mut store, "R1", fetched("mode: rule\n"), Vec::new()).unwrap();
        let outcome =
            store_fetched(&mut store, "R1", fetched("mode: global\n"), Vec::new()).unwrap();
        assert!(!outcome.unchanged);
        let item = store.get("R1").unwrap();
        assert_eq!(store.read_document(item).unwrap(), "mode: global\n");
    }

    #[test]
    fn the_stored_document_is_what_decides_unchanged() {
        let (_dir, store) = store_with_remote("R1", "https://example.com/sub");
        let item = store.get("R1").unwrap().clone();
        assert!(
            !document_matches(&store, &item, "mode: rule\n"),
            "a missing document is never current"
        );
        store.write_document(&item, "mode: rule\n").unwrap();
        assert!(document_matches(&store, &item, "mode: rule\n"));
        assert!(!document_matches(&store, &item, "mode: global\n"));
    }

    #[test]
    fn a_failed_document_write_leaves_the_index_claiming_nothing() {
        let dir = TempDir::new().unwrap();
        let paths = AppPaths::new(dir.path());
        paths.ensure_dirs().unwrap();
        let mut store = ProfileStore::load(&paths).unwrap();
        store.add(PrfItem::remote("R1", "Airport", "https://example.com/sub"));
        store.save().unwrap();

        // Replace the profiles directory with a file so the document cannot be
        // written: this is the failure the ordering rule exists for.
        std::fs::remove_dir_all(paths.profiles_dir()).unwrap();
        std::fs::write(paths.profiles_dir(), b"not a directory").unwrap();

        let err = store_fetched(&mut store, "R1", fetched("mode: rule\n"), Vec::new()).unwrap_err();
        assert!(matches!(err, Error::Io { .. }), "{err:?}");
        assert!(
            store.get("R1").unwrap().updated.is_none(),
            "the in-memory index must not move either"
        );
        let reloaded = ProfileStore::load(&paths).unwrap();
        assert!(reloaded.get("R1").unwrap().updated.is_none());
    }

    #[test]
    fn an_outcome_summarises_the_route_and_the_fallbacks() {
        let outcome = UpdateOutcome {
            uid: "R1".to_owned(),
            bytes: 42,
            user_info: UserInfo::default(),
            source: FetchSource::ViaClashProxy("http://127.0.0.1:7890".to_owned()),
            suggested_name: None,
            unchanged: true,
            attempts: vec![
                Attempt::failed(FetchSource::Direct, &Error::invalid("url", "no route")),
                Attempt::succeeded(FetchSource::ViaClashProxy(
                    "http://127.0.0.1:7890".to_owned(),
                )),
            ],
        };
        let summary = outcome.summary();
        assert!(summary.contains("R1"), "{summary}");
        assert!(summary.contains("42 bytes"), "{summary}");
        assert!(
            summary.contains("clash proxy http://127.0.0.1:7890"),
            "{summary}"
        );
        assert!(summary.contains("unchanged"), "{summary}");
        assert!(summary.contains("1 failed tier"), "{summary}");
        assert_eq!(outcome.failed_attempts(), 1);
        assert!(!outcome.attempts[0].ok);
        assert_eq!(
            outcome.attempts[0].error.as_deref(),
            Some("invalid value for url: no route")
        );
    }

    #[tokio::test]
    async fn nothing_is_due_in_an_empty_store() {
        let (_dir, mut store) = store_with_remote("R1", "https://example.com/sub");
        let fetcher = SubscriptionFetcher::new(None).unwrap();
        assert!(
            fetcher.update_all_due(&mut store).await.is_empty(),
            "a profile with no interval is never swept up"
        );

        store.get_mut("R1").unwrap().option = PrfOption {
            update_interval: Some(60),
            allow_auto_update: Some(true),
            ..PrfOption::default()
        };
        // It is due now, and the sweep reaches it — the fetch itself fails
        // because there is no network in this test, which is exactly the
        // per-profile result the caller is meant to render.
        store.get_mut("R1").unwrap().url = Some("file:///nope".to_owned());
        let results = fetcher.update_all_due(&mut store).await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "R1");
        assert!(results[0].1.is_err());
    }
}
