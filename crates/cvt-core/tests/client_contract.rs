//! End-to-end contract tests for [`cvt_core::mihomo::client::Client`].
//!
//! Every test here drives the *real* client against a hand-rolled HTTP/1.1
//! controller built on a `tokio` listener. The server reads one request per
//! connection, hands it to a closure, records it, and writes a canned
//! response — which is exactly what lets these tests assert on **the raw bytes
//! the client sent**: the request target (including percent-encoding and the
//! query string), the headers, and the body.
//!
//! Response shapes are copied verbatim from `research/mihomo-api.md`, which is
//! the specification this client is held to. Where a shape contradicts the
//! official docs (PascalCase rule types, string ports, `"connections": null`,
//! a `0` in a group delay map, `GET /providers/rules/{name}` being absent), the
//! spec wins and the test says so.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cvt_core::error::Error;
use cvt_core::mihomo::client::Client;
use cvt_core::mihomo::endpoint::Endpoint;
use cvt_core::mihomo::stream::{Options, Selection, Stream};
use cvt_core::mihomo::types::{ConfigPatch, LogLevel};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

// ---------------------------------------------------------------- the server

/// One request as it arrived on the wire.
#[derive(Debug, Clone)]
struct Request {
    method: String,
    /// The raw request target, e.g.
    /// `/proxies/JP%2001/delay?url=https%3A%2F%2Fx&timeout=5000`.
    target: String,
    version: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    fn bearer(&self) -> Option<&str> {
        self.header("authorization")
            .and_then(|v| v.strip_prefix("Bearer "))
    }

    /// The path, without the query string.
    fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or("")
    }

    /// The query string, without the leading `?`.
    fn query(&self) -> &str {
        self.target.split_once('?').map_or("", |(_, q)| q)
    }

    fn body_str(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    fn line(&self) -> String {
        format!("{} {}", self.method, self.target)
    }
}

/// A canned response.
#[derive(Debug, Clone)]
struct Response {
    status: u16,
    content_type: Option<&'static str>,
    body: Vec<u8>,
    /// How long the server waits before answering, for testing client-side
    /// deadlines.
    delay: Duration,
}

impl Response {
    fn json(body: &str) -> Self {
        Self::json_status(200, body)
    }

    /// Every JSON body a real core writes ends with a trailing newline
    /// (`chi/render` uses `json.Encoder`), so the fake controller does too.
    fn json_status(status: u16, body: &str) -> Self {
        Self {
            status,
            content_type: Some("application/json"),
            body: format!("{body}\n").into_bytes(),
            delay: Duration::ZERO,
        }
    }

    fn text(status: u16, body: &str) -> Self {
        Self {
            status,
            content_type: Some("text/plain; charset=utf-8"),
            body: body.as_bytes().to_vec(),
            delay: Duration::ZERO,
        }
    }

    /// The core's answer for a path this build does not have.
    fn route_absent() -> Self {
        Self::text(404, "404 page not found\n")
    }

    /// A method mismatch: `405`, an `Allow` header, and a zero-length body.
    fn method_not_allowed() -> Self {
        Self::empty(405)
    }

    fn no_content() -> Self {
        Self::empty(204)
    }

    fn empty(status: u16) -> Self {
        Self {
            status,
            content_type: None,
            body: Vec::new(),
            delay: Duration::ZERO,
        }
    }

    /// The literal `null` body `/storage/{key}` uses for a missing key — note
    /// the spec's warning that this one response has **no** trailing newline.
    fn raw_json(body: &'static str) -> Self {
        Self {
            status: 200,
            content_type: Some("application/json"),
            body: body.as_bytes().to_vec(),
            delay: Duration::ZERO,
        }
    }

    fn after(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }
}

type Handler = Arc<dyn Fn(&Request) -> Response + Send + Sync + 'static>;

fn handler(f: impl Fn(&Request) -> Response + Send + Sync + 'static) -> Handler {
    Arc::new(f)
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Content Too Large",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Status",
    }
}

/// Read one request off a connection, record it, answer it, close.
async fn serve_connection<S>(mut socket: S, handler: Handler, log: Arc<Mutex<Vec<Request>>>)
where
    S: AsyncReadExt + AsyncWriteExt + Unpin,
{
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(pos) = find(&buf, b"\r\n\r\n") {
            break pos + 4;
        }
        match socket.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
        if buf.len() > 1 << 20 {
            return;
        }
    };

    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split(' ');
    let method = parts.next().unwrap_or_default().to_owned();
    let target = parts.next().unwrap_or_default().to_owned();
    let version = parts.next().unwrap_or_default().to_owned();

    let mut headers: Vec<(String, String)> = Vec::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_owned()));
        }
    }

    let want: usize = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = buf[head_end..].to_vec();
    while body.len() < want {
        match socket.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => body.extend_from_slice(&chunk[..n]),
        }
    }
    body.truncate(want);

    let request = Request {
        method,
        target,
        version,
        headers,
        body,
    };
    let response = handler(&request);
    log.lock().unwrap().push(request);

    if !response.delay.is_zero() {
        tokio::time::sleep(response.delay).await;
    }

    let mut out = format!(
        "HTTP/1.1 {} {}\r\n",
        response.status,
        reason(response.status)
    );
    if let Some(ct) = response.content_type {
        out.push_str(&format!("Content-Type: {ct}\r\n"));
    }
    out.push_str(&format!("Content-Length: {}\r\n", response.body.len()));
    out.push_str("Connection: close\r\n\r\n");
    let _ = socket.write_all(out.as_bytes()).await;
    let _ = socket.write_all(&response.body).await;
    let _ = socket.flush().await;
    let _ = socket.shutdown().await;
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// A controller listening on an ephemeral TCP port.
struct FakeController {
    addr: String,
    log: Arc<Mutex<Vec<Request>>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl FakeController {
    async fn start(handler: Handler) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let log: Arc<Mutex<Vec<Request>>> = Arc::new(Mutex::new(Vec::new()));
        let task = tokio::spawn({
            let log = Arc::clone(&log);
            async move {
                loop {
                    let Ok((socket, _)) = listener.accept().await else {
                        break;
                    };
                    let handler = Arc::clone(&handler);
                    let log = Arc::clone(&log);
                    tokio::spawn(async move { serve_connection(socket, handler, log).await });
                }
            }
        });
        Self {
            addr,
            log,
            task: Some(task),
        }
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::tcp(self.addr.clone(), None)
    }

    fn endpoint_with_secret(&self, secret: &str) -> Endpoint {
        Endpoint::tcp(self.addr.clone(), Some(secret.to_owned()))
    }

    fn client(&self) -> Client {
        Client::new(self.endpoint()).unwrap()
    }

    fn client_with_secret(&self, secret: &str) -> Client {
        Client::new(self.endpoint_with_secret(secret)).unwrap()
    }

    /// Every request seen so far, in arrival order.
    fn seen(&self) -> Vec<Request> {
        self.log.lock().unwrap().clone()
    }

    fn count(&self) -> usize {
        self.log.lock().unwrap().len()
    }

    fn clear(&self) {
        self.log.lock().unwrap().clear();
    }

    /// The single request recorded since the last [`FakeController::clear`].
    fn only(&self) -> Request {
        let seen = self.seen();
        assert_eq!(
            seen.len(),
            1,
            "expected exactly one request, saw: {:?}",
            seen.iter().map(Request::line).collect::<Vec<_>>()
        );
        seen.into_iter().next().unwrap()
    }
}

impl Drop for FakeController {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// A controller listening on a unix socket.
#[cfg(unix)]
struct FakeUnixController {
    path: std::path::PathBuf,
    log: Arc<Mutex<Vec<Request>>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

#[cfg(unix)]
impl FakeUnixController {
    async fn start(handler: Handler, dir: &std::path::Path) -> Self {
        let path = dir.join("mihomo.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let log: Arc<Mutex<Vec<Request>>> = Arc::new(Mutex::new(Vec::new()));
        let task = tokio::spawn({
            let log = Arc::clone(&log);
            async move {
                loop {
                    let Ok((socket, _)) = listener.accept().await else {
                        break;
                    };
                    let handler = Arc::clone(&handler);
                    let log = Arc::clone(&log);
                    tokio::spawn(async move { serve_connection(socket, handler, log).await });
                }
            }
        });
        Self {
            path,
            log,
            task: Some(task),
        }
    }

    fn client(&self) -> Client {
        Client::new(Endpoint::unix(self.path.clone())).unwrap()
    }
}

#[cfg(unix)]
impl Drop for FakeUnixController {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Percent-decode, for checking that an encoded segment round-trips.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 3 <= bytes.len() {
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

// -------------------------------------------------- the controller's answers

const VERSION: &str = r#"{"meta":true,"version":"v1.19.31"}"#;
const HELLO: &str = r#"{"hello":"mihomo"}"#;

/// A real `Direct` proxy, verbatim from the spec (§7.1).
const DIRECT: &str = r#"{"alive":true,"dialer-proxy":"","extra":{},"history":[],
  "id":"fae47b09-9950-4ee3-be48-fb04f83906d7","interface":"","mptcp":false,
  "name":"DIRECT","provider-name":"","routing-mark":0,"smux":false,"tfo":false,
  "type":"Direct","udp":true,"uot":false,"xudp":false}"#;

/// A real `Selector` group, verbatim from the spec (§7.2).
const SELECTOR: &str = r#"{"alive":true,"all":["node-a","node-b","DIRECT"],
  "dialer-proxy":"","emptyFallback":"COMPATIBLE","extra":{},"hidden":false,
  "history":[],"icon":"","interface":"","mptcp":false,"name":"grp-select",
  "now":"node-a","provider-name":"","routing-mark":0,"smux":false,"testUrl":"",
  "tfo":false,"type":"Selector","udp":false,"uot":false,"xudp":false}"#;

/// The real in-flight connection captured live (spec §5.3).
///
/// `/connections` frames are newline-delimited, so the body must contain no
/// raw newline: this is `concat!`ed into one line, exactly as the core writes
/// it.
const CONNECTIONS: &str = concat!(
    r#"{"downloadTotal":0,"uploadTotal":87,"#,
    r#""connections":[{"id":"14ddd52c-5d8c-4ece-afd3-7125c0637db4","#,
    r#""metadata":{"network":"tcp","type":"HTTP","sourceIP":"127.0.0.1","#,
    r#""destinationIP":"127.0.0.1","sourceGeoIP":null,"destinationGeoIP":null,"#,
    r#""sourceIPASN":"","destinationIPASN":"","sourcePort":"50130","#,
    r#""destinationPort":"18099","inboundIP":"127.0.0.1","inboundPort":"17890","#,
    r#""inboundName":"DEFAULT-MIXED","inboundUser":"","rematchName":"","#,
    r#""host":"","dnsMode":"normal","uid":0,"process":"","processPath":"","#,
    r#""specialProxy":"","specialRules":"","remoteDestination":"127.0.0.1","#,
    r#""dscp":0,"sniffHost":""},"#,
    r#""upload":87,"download":0,"start":"2026-09-25T17:40:55.786450952+08:00","#,
    r#""chains":["DIRECT","grp-select"],"providerChains":["",""],"#,
    r#""rule":"Match","rulePayload":""}],"#,
    r#""memory":0}"#,
);

const RULES: &str = r#"{"rules":[{"index":0,"type":"DomainSuffix","payload":"example.com",
  "proxy":"grp-select","size":-1,"extra":{"disabled":false,"hitCount":0,
  "hitAt":"1970-01-01T07:30:00+07:30","missCount":0,"missAt":"1970-01-01T07:30:00+07:30"}},
  {"index":1,"type":"Match","payload":"","proxy":"DIRECT","size":-1}]}"#;

const RULE_PROVIDERS: &str = r#"{"providers":{"rp-inline":{"behavior":"Domain","format":"","name":"rp-inline",
  "ruleCount":2,"type":"Rule","vehicleType":"Inline",
  "updatedAt":"2026-09-25T17:33:42.768369569+08:00","payload":["example.com","example.org"]}}}"#;

const PROXY_PROVIDERS: &str = r#"{"providers":{"default":{"name":"default","type":"Proxy",
  "vehicleType":"Compatible","proxies":[],"testUrl":"","expectedStatus":"*",
  "updatedAt":"0001-01-01T00:00:00Z"}}}"#;

/// A route table that answers every endpoint the client knows, the way the
/// spec says the live core does.
fn live_core(request: &Request) -> Response {
    match (request.method.as_str(), request.path()) {
        ("GET", "/") => Response::json(HELLO),
        ("GET", "/version") => Response::json(VERSION),
        ("GET", "/proxies") => Response::json(&format!(
            r#"{{"proxies":{{"DIRECT":{DIRECT},"grp-select":{SELECTOR}}}}}"#
        )),
        ("GET", "/group") => Response::json(&format!(r#"{{"proxies":[{SELECTOR}]}}"#)),
        ("GET", "/providers/proxies") => Response::json(PROXY_PROVIDERS),
        ("GET", "/providers/rules") => Response::json(RULE_PROVIDERS),
        ("GET", "/rules") => Response::json(RULES),
        ("GET", "/connections") => Response::json(CONNECTIONS),
        ("GET", "/configs") => Response::json(r#"{"mixed-port":17890,"mode":"rule"}"#),
        ("GET", "/traffic") => Response::json(r#"{"up":1,"down":2,"upTotal":3,"downTotal":4}"#),
        ("GET", "/memory") => Response::json(r#"{"inuse":50282496,"oslimit":0}"#),
        ("GET", "/logs") => Response::json("{\"type\":\"info\",\"payload\":\"hello\"}\n"),
        ("GET", "/dns/query") => Response::json(
            r#"{"Status":0,"Answer":[{"name":"example.com.","type":1,"TTL":1,"data":"198.18.0.41"}]}"#,
        ),
        ("GET", "/storage/absent") => Response::raw_json("null"),
        ("GET", "/storage/plain") => Response::json(r#"{"remembered":true}"#),
        ("POST", "/restart" | "/upgrade" | "/upgrade/ui") => Response::json(r#"{"status":"ok"}"#),
        ("PUT", "/debug/gc") => Response::empty(200),
        ("GET", p) if p.starts_with("/proxies/") && p.ends_with("/delay") => {
            Response::json(r#"{"delay":42}"#)
        }
        ("GET", p) if p.starts_with("/group/") && p.ends_with("/delay") => {
            Response::json(r#"{"DIRECT":0,"node-a":42}"#)
        }
        ("GET", p) if p.starts_with("/proxies/") => Response::json(DIRECT),
        ("GET", p) if p.starts_with("/group/") => Response::json(SELECTOR),
        ("GET", p) if p.starts_with("/providers/proxies/") && p.ends_with("/healthcheck") => {
            Response::no_content()
        }
        ("GET", p) if p.starts_with("/providers/proxies/") => {
            Response::json(r#"{"name":"p1","type":"Proxy","vehicleType":"HTTP","proxies":[]}"#)
        }
        _ => Response::no_content(),
    }
}

// ------------------------------------------------------- 1. exact requests

#[tokio::test]
async fn every_read_method_sends_the_documented_request() {
    let core = FakeController::start(handler(live_core)).await;
    let client = core.client();

    client.hello().await.unwrap();
    client.version().await.unwrap();
    assert!(client.is_reachable().await);
    client.proxies().await.unwrap();
    client.proxy("JP 01").await.unwrap();
    client.groups().await.unwrap();
    client.group("广告 01").await.unwrap();
    client.proxy_providers().await.unwrap();
    client.proxy_provider("prov one").await.unwrap();
    client.proxy_provider_healthcheck("prov one").await.unwrap();
    client.rule_providers().await.unwrap();
    client.rules().await.unwrap();
    client.connections().await.unwrap();
    client.configs().await.unwrap();
    client.storage_get("absent").await.unwrap();
    client.dns_query("example.com", "A").await.unwrap();

    let lines: Vec<String> = core.seen().iter().map(Request::line).collect();
    assert_eq!(
        lines,
        vec![
            "GET /".to_owned(),
            "GET /version".to_owned(),
            "GET /".to_owned(),
            "GET /proxies".to_owned(),
            "GET /proxies/JP%2001".to_owned(),
            "GET /group".to_owned(),
            "GET /group/%E5%B9%BF%E5%91%8A%2001".to_owned(),
            "GET /providers/proxies".to_owned(),
            "GET /providers/proxies/prov%20one".to_owned(),
            "GET /providers/proxies/prov%20one/healthcheck".to_owned(),
            "GET /providers/rules".to_owned(),
            "GET /rules".to_owned(),
            "GET /connections".to_owned(),
            "GET /configs".to_owned(),
            "GET /storage/absent".to_owned(),
            "GET /dns/query?name=example.com&type=A".to_owned(),
        ],
        "the client must send exactly these request lines"
    );
    for request in core.seen() {
        assert_eq!(request.version, "HTTP/1.1", "{}", request.target);
        assert_eq!(request.header("accept"), Some("application/json"));
        assert!(
            request.bearer().is_none(),
            "no secret is configured, so no Authorization header may be sent"
        );
    }
}

#[tokio::test]
async fn percent_encoding_of_names_matches_what_the_core_unescapes() {
    let core = FakeController::start(handler(live_core)).await;
    let client = core.client();

    let names = ["JP 01", "node b/slash", "50%", "a&b=c", "emoji-🇯🇵"];
    client.proxy(names[0]).await.unwrap();
    client.proxy(names[1]).await.unwrap();
    client.group(names[2]).await.unwrap();
    client.group(names[3]).await.unwrap();
    client.group(names[4]).await.unwrap();

    let seen = core.seen();
    let paths: Vec<&str> = seen.iter().map(Request::path).collect();
    assert_eq!(
        paths,
        vec![
            "/proxies/JP%2001",
            "/proxies/node%20b%2Fslash",
            "/group/50%25",
            "/group/a%26b%3Dc",
            "/group/emoji-%F0%9F%87%AF%F0%9F%87%B5",
        ],
        "mihomo unescapes each segment once, so `/` must be `%2F` and ` ` must be `%20`"
    );
    for (request, name) in seen.iter().zip(names) {
        let segment = request.path().rsplit('/').next().unwrap();
        assert_eq!(percent_decode(segment), name, "{}", request.target);
    }
}

#[tokio::test]
async fn delay_endpoints_send_url_timeout_and_expected_exactly() {
    let core = FakeController::start(handler(live_core)).await;
    let client = core.client();

    client
        .proxy_delay(
            "JP 01",
            "https://gstatic.com/generate_204",
            5000,
            Some("200/204"),
        )
        .await
        .unwrap();
    client
        .proxy_delay("JP 01", "https://x.test/204", 1000, None)
        .await
        .unwrap();
    client
        .proxy_delay("JP 01", "https://x.test/204", 1000, Some(""))
        .await
        .unwrap();
    client
        .group_delay(
            "grp-select",
            "http://cp.cloudflare.com/generate_204",
            32767,
            Some("*"),
        )
        .await
        .unwrap();

    let targets: Vec<String> = core.seen().iter().map(|r| r.target.clone()).collect();
    assert_eq!(
        targets[0],
        "/proxies/JP%2001/delay?url=https%3A%2F%2Fgstatic.com%2Fgenerate_204&timeout=5000&expected=200%2F204"
    );
    assert_eq!(targets[0].split('&').count(), 3);
    assert_eq!(
        targets[1],
        "/proxies/JP%2001/delay?url=https%3A%2F%2Fx.test%2F204&timeout=1000"
    );
    assert_eq!(
        targets[2], targets[1],
        "an empty `expected` must be omitted, not sent as `expected=`"
    );
    assert_eq!(
        targets[3],
        "/group/grp-select/delay?url=http%3A%2F%2Fcp.cloudflare.com%2Fgenerate_204&timeout=32767&expected=%2A",
        "the core unescapes the query, so `*` may be encoded"
    );
}

#[tokio::test]
async fn writes_send_the_exact_json_bodies_the_core_expects() {
    let core = FakeController::start(handler(live_core)).await;
    let client = core.client();

    client.select("grp-select", "node b/slash").await.unwrap();
    client.clear_selection("grp-select").await.unwrap();
    client.update_proxy_provider("prov one").await.unwrap();
    client.update_rule_provider("rp 1").await.unwrap();
    client.close_all_connections().await.unwrap();
    client.close_connection("14ddd52c 5d8c").await.unwrap();
    client.update_geo().await.unwrap();
    client.flush_fakeip().await.unwrap();
    client.flush_dns().await.unwrap();
    client.upgrade_geo().await.unwrap();
    client.upgrade_ui().await.unwrap();
    client.restart().await.unwrap();
    client.force_gc().await.unwrap();
    client
        .storage_put("plain", &json!({"a": [1, 2]}))
        .await
        .unwrap();
    client.storage_delete("plain").await.unwrap();

    let seen = core.seen();
    let lines: Vec<String> = seen.iter().map(Request::line).collect();
    assert_eq!(
        lines,
        vec![
            "PUT /proxies/grp-select",
            "DELETE /proxies/grp-select",
            "PUT /providers/proxies/prov%20one",
            "PUT /providers/rules/rp%201",
            "DELETE /connections",
            "DELETE /connections/14ddd52c%205d8c",
            "POST /configs/geo",
            "POST /cache/fakeip/flush",
            "POST /cache/dns/flush",
            "POST /upgrade/geo",
            "POST /upgrade/ui",
            "POST /restart",
            "PUT /debug/gc",
            "PUT /storage/plain",
            "DELETE /storage/plain",
        ]
    );

    assert_eq!(
        seen[0].body_str(),
        r#"{"name":"node b/slash"}"#,
        "PUT /proxies/{{name}} takes the member name under `name`"
    );
    assert_eq!(
        seen[0].header("content-type"),
        Some("application/json"),
        "a JSON body must declare its type"
    );
    assert!(seen[1].body.is_empty(), "DELETE carries no body");
    assert_eq!(seen[13].body_str(), r#"{"a":[1,2]}"#);
    assert!(seen[14].body.is_empty(), "DELETE carries no body");
}

#[tokio::test]
async fn rules_disable_uses_string_indices_as_the_documented_body() {
    let core = FakeController::start(handler(live_core)).await;
    let client = core.client();

    let mut changes = BTreeMap::new();
    changes.insert(0u32, true);
    changes.insert(3u32, false);
    client.set_rules_disabled(&changes).await.unwrap();

    let request = core.only();
    assert_eq!(request.method, "PATCH");
    assert_eq!(request.path(), "/rules/disable");
    assert_eq!(
        request.body_str(),
        r#"{"0":true,"3":false}"#,
        "keys are rule indices *as strings*; a `{{\"disabled\":true}}` body does nothing"
    );

    core.clear();
    client.set_rules_disabled(&BTreeMap::new()).await.unwrap();
    assert_eq!(core.count(), 0, "an empty change set must not hit the wire");
}

#[tokio::test]
async fn patch_and_put_configs_send_omitted_fields_as_absent() {
    let core = FakeController::start(handler(live_core)).await;
    let client = core.client();

    let patch = ConfigPatch {
        mode: Some("global".to_owned()),
        log_level: Some("warning".to_owned()),
        mixed_port: Some(17890),
        ..ConfigPatch::default()
    };
    client.patch_configs(&patch).await.unwrap();
    let request = core.only();
    assert_eq!(request.method, "PATCH");
    assert_eq!(request.path(), "/configs");
    assert_eq!(
        request.body_str(),
        r#"{"mode":"global","log-level":"warning","mixed-port":17890}"#,
        "a field left as None means 'do not touch it', so it must be absent"
    );

    core.clear();
    client.patch_configs(&ConfigPatch::default()).await.unwrap();
    assert_eq!(core.count(), 0, "an empty patch must not hit the wire");

    core.clear();
    client
        .reload_configs(
            Some(std::path::Path::new("/tmp/mh/config.yaml")),
            None,
            true,
        )
        .await
        .unwrap();
    let request = core.only();
    assert_eq!(request.method, "PUT");
    assert_eq!(request.target, "/configs?force=true");
    assert_eq!(request.body_str(), r#"{"path":"/tmp/mh/config.yaml"}"#);

    core.clear();
    client
        .reload_configs(None, Some("mixed-port: 7890\n"), false)
        .await
        .unwrap();
    let request = core.only();
    assert_eq!(request.target, "/configs");
    assert_eq!(request.body_str(), r#"{"payload":"mixed-port: 7890\n"}"#);

    core.clear();
    client.reload_configs(None, None, false).await.unwrap();
    assert_eq!(core.only().body_str(), r#"{}"#);
}

#[tokio::test]
async fn upgrade_queries_build_and_trim_correctly() {
    let core = FakeController::start(handler(live_core)).await;
    let client = core.client();

    client.upgrade_core(Some("release"), true).await.unwrap();
    client.upgrade_core(Some("alpha"), false).await.unwrap();
    client.upgrade_core(None, true).await.unwrap();
    client.upgrade_core(None, false).await.unwrap();
    client.upgrade_core(Some(""), false).await.unwrap();

    let targets: Vec<String> = core.seen().iter().map(|r| r.target.clone()).collect();
    assert_eq!(
        targets,
        vec![
            "/upgrade?channel=release&force=true",
            "/upgrade?channel=alpha",
            "/upgrade?force=true",
            "/upgrade",
            "/upgrade",
        ],
        "an absent channel and no force must leave a bare path, not a dangling `?`"
    );
}

#[tokio::test]
async fn rule_provider_is_looked_up_from_the_collection_not_a_sub_path() {
    let core = FakeController::start(handler(live_core)).await;
    let client = core.client();

    let provider = client.rule_provider("rp-inline").await.unwrap();
    assert_eq!(provider.rule_count, 2);
    assert_eq!(
        core.only().target,
        "/providers/rules",
        "GET /providers/rules/{{name}} answers 405 on every build, so it must never be requested"
    );

    core.clear();
    let err = client.rule_provider("nope").await.unwrap_err();
    assert_eq!(core.count(), 1, "the collection is fetched once");
    match err {
        Error::Api { status, .. } => assert_eq!(status, 404),
        other => panic!("expected a 404 Api error, got {other:?}"),
    }
}

#[tokio::test]
async fn a_secret_is_sent_as_a_bearer_token_only_when_configured() {
    let core = FakeController::start(handler(live_core)).await;

    let open = core.client();
    open.version().await.unwrap();
    assert!(
        core.only().bearer().is_none(),
        "sending a header the user did not configure is wrong"
    );

    core.clear();
    let authed = core.client_with_secret("hunter2");
    authed.version().await.unwrap();
    assert_eq!(core.only().bearer(), Some("hunter2"));

    // A blank secret must not become `Authorization: Bearer `.
    core.clear();
    let blank = core.client_with_secret("   ");
    blank.version().await.unwrap();
    assert!(core.only().bearer().is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn the_unix_transport_works_and_carries_no_secret() {
    let dir = tempfile::TempDir::new().unwrap();
    let core = FakeUnixController::start(handler(live_core), dir.path()).await;
    let client = core.client();

    assert_eq!(client.endpoint().base_url(), "http://localhost");
    assert!(
        !client.endpoint().is_authenticated(),
        "the core builds socket listeners with an empty secret"
    );

    let version = client.version().await.unwrap();
    assert_eq!(version.trimmed(), "1.19.31");
    client.proxies().await.unwrap();

    let seen = core.log.lock().unwrap().clone();
    assert_eq!(seen.len(), 2, "the socket must actually be used");
    assert_eq!(seen[0].target, "/version");
    assert!(
        seen[0].bearer().is_none(),
        "the core does not check a secret on this transport, and none is configured"
    );
}

// --------------------------------------------------- 2. response decoding

#[tokio::test]
async fn decodes_the_real_version_and_liveness_shapes() {
    let core = FakeController::start(handler(live_core)).await;
    let client = core.client();

    // `/version` has exactly two keys and no `premium` (spec §2.2).
    let v = client.version().await.unwrap();
    assert!(v.meta);
    assert_eq!(v.version, "v1.19.31");
    assert_eq!(v.trimmed(), "1.19.31");
    assert_eq!(v.semver(), Some((1, 19, 31)));

    assert_eq!(client.hello().await.unwrap().hello, "mihomo");
}

#[tokio::test]
async fn decodes_a_real_selector_group_and_a_real_direct_proxy() {
    let core = FakeController::start(handler(live_core)).await;
    let client = core.client();

    let proxies = client.proxies().await.unwrap();
    assert_eq!(proxies.proxies.len(), 2);

    let group = proxies.proxies.get("grp-select").unwrap();
    assert!(group.is_group(), "`all` is what makes it a group");
    assert_eq!(group.kind, "Selector");
    assert_eq!(group.members(), ["node-a", "node-b", "DIRECT"]);
    assert_eq!(group.now.as_deref(), Some("node-a"));
    assert!(group.is_selectable());
    assert!(group.id.is_none(), "groups carry no id (spec §10.13)");

    let direct = proxies.proxies.get("DIRECT").unwrap();
    assert!(!direct.is_group());
    assert_eq!(direct.kind, "Direct");
    assert!(!direct.is_selectable());
    assert!(direct.extra.contains_key("xudp"), "unknown keys survive");

    let groups = client.groups().await.unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].name, "grp-select");

    assert_eq!(client.proxy("DIRECT").await.unwrap().kind, "Direct");
    assert!(client.group("grp-select").await.unwrap().is_group());
}

#[tokio::test]
async fn decodes_connections_including_null_and_a_real_in_flight_connection() {
    let core = FakeController::start(handler(live_core)).await;
    let client = core.client();
    let snapshot = client.connections().await.unwrap();
    assert_eq!(snapshot.items().len(), 1);
    let conn = &snapshot.items()[0];
    assert_eq!(conn.total(), 87);
    assert_eq!(conn.upload, 87);
    assert_eq!(conn.selected_group(), Some("grp-select"));
    assert_eq!(conn.outbound_node(), None, "DIRECT is not a node");
    assert_eq!(conn.rule_label(), "MATCH");
    assert_eq!(conn.meta().destination_port_num(), Some(18099));
    assert_eq!(conn.meta().source_port_num(), Some(50130));

    // `"connections": null` is what an idle core sends — not `[]`.
    let empty = FakeController::start(handler(|_| {
        Response::json(r#"{"downloadTotal":0,"uploadTotal":0,"connections":null,"memory":0}"#)
    }))
    .await;
    let snapshot = empty.client().connections().await.unwrap();
    assert!(snapshot.items().is_empty());
    assert!(snapshot.connections.is_none());
    assert_eq!(snapshot.total_bytes(), 0);
}

#[tokio::test]
async fn decodes_rules_with_pascal_case_types_and_their_counters() {
    let core = FakeController::start(handler(live_core)).await;
    let client = core.client();

    let rules = client.rules().await.unwrap();
    assert_eq!(rules.len(), 2);
    assert_eq!(rules[0].kind, "DomainSuffix");
    assert_eq!(
        rules[0].as_config_rule(),
        "DOMAIN-SUFFIX,example.com,grp-select"
    );
    assert_eq!(rules[1].as_config_rule(), "MATCH,DIRECT");
    assert_eq!(rules[0].size, -1);
    assert_eq!(rules[0].stats().evaluations(), 0);
    assert!(
        rules[1].extra.is_none(),
        "a rule the core does not wrap carries no counters"
    );
}

#[tokio::test]
async fn decodes_both_provider_collections() {
    let core = FakeController::start(handler(live_core)).await;
    let client = core.client();

    let rules = client.rule_providers().await.unwrap();
    let rp = rules.providers.get("rp-inline").unwrap();
    assert_eq!(rp.behavior, "Domain");
    assert_eq!(rp.format, "", "inline providers never set `format`");
    assert_eq!(rp.rule_count, 2);
    assert_eq!(rp.vehicle_type, "Inline");
    assert_eq!(rp.payload, ["example.com", "example.org"]);

    let proxies = client.proxy_providers().await.unwrap();
    let pp = proxies.providers.get("default").unwrap();
    assert_eq!(pp.kind, "Proxy");
    assert!(
        pp.is_synthetic(),
        "Compatible/Inline providers cannot refresh"
    );
    assert!(pp.never_updated(), "the Go zero time means never updated");
}

#[tokio::test]
async fn decodes_configs_and_storage_bodies() {
    let core = FakeController::start(handler(live_core)).await;
    let client = core.client();

    let cfg = client.configs().await.unwrap();
    assert_eq!(cfg.mixed_port, Some(17890));
    assert_eq!(cfg.mode.as_deref(), Some("rule"));

    // A literal `null` body means "no such key", and must not be an error.
    assert!(client.storage_get("absent").await.unwrap().is_none());
    assert_eq!(
        client.storage_get("plain").await.unwrap(),
        Some(json!({"remembered": true}))
    );

    // `/dns/query` has no typed model: it must round-trip as a raw value.
    let answer = client.dns_query("example.com", "A").await.unwrap();
    assert_eq!(answer["Answer"][0]["data"], json!("198.18.0.41"));
}

#[tokio::test]
async fn a_group_delay_map_may_legitimately_contain_zero() {
    let core = FakeController::start(handler(live_core)).await;
    let client = core.client();

    let delays = client
        .group_delay("grp-select", "http://x/204", 5000, None)
        .await
        .unwrap();
    assert_eq!(delays.get("DIRECT"), Some(&0), "0 is valid in a group map");
    assert_eq!(delays.get("node-a"), Some(&42));
    assert_eq!(delays.get("node-b"), None, "failures are simply absent");

    assert_eq!(
        client
            .proxy_delay("DIRECT", "http://x/204", 5000, None)
            .await
            .unwrap(),
        42
    );
}

// ------------------------------------------- 3. streams (`/logs` and friends)

/// Drive a stream built from `endpoint`/`options` until `want` payload events
/// have arrived, and report the request targets the fake controller saw.
async fn drive(
    core: &FakeController,
    endpoint: Endpoint,
    options: Options,
    want: usize,
) -> (Vec<String>, Vec<String>) {
    let mut stream = Stream::spawn(endpoint, options).unwrap();
    let mut names: Vec<String> = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while names.len() < want && tokio::time::Instant::now() < deadline {
        if let Some(event) = stream.recv_timeout(Duration::from_millis(500)).await
            && !event.is_status()
        {
            names.push(event.name().to_owned());
        }
    }
    names.sort();
    let mut targets: Vec<String> = core.seen().iter().map(|r| r.target.clone()).collect();
    targets.sort();
    targets.dedup();
    (targets, names)
}

#[tokio::test]
async fn the_log_stream_requests_the_level_it_was_configured_with() {
    let core = FakeController::start(handler(live_core)).await;
    let selection = Selection {
        logs: true,
        ..Selection::none()
    };
    let options = Options::new(selection)
        .with_http_only()
        .with_log_level(LogLevel::Warning);
    let (targets, names) = drive(&core, core.endpoint(), options, 1).await;

    assert_eq!(
        targets,
        vec!["/logs?level=warning".to_owned()],
        "the level must be the core's spelling (`warning`, never `warn`)"
    );
    assert_eq!(names, vec!["logs"]);
}

#[tokio::test]
async fn the_connections_stream_asks_for_its_interval() {
    let core = FakeController::start(handler(live_core)).await;
    let mut options = Options::new(Selection {
        connections: true,
        ..Selection::none()
    })
    .with_http_only();
    options.connection_interval = Duration::from_millis(1500);
    let (targets, names) = drive(&core, core.endpoint(), options, 1).await;

    assert_eq!(targets, vec!["/connections?interval=1500".to_owned()]);
    assert_eq!(names, vec!["connections"]);
}

#[tokio::test]
async fn the_traffic_and_memory_streams_use_their_bare_paths() {
    let core = FakeController::start(handler(live_core)).await;
    let options = Options::new(Selection {
        traffic: true,
        memory: true,
        ..Selection::none()
    })
    .with_http_only();
    let (targets, names) = drive(&core, core.endpoint(), options, 2).await;

    assert_eq!(targets, vec!["/memory".to_owned(), "/traffic".to_owned()]);
    assert_eq!(names, vec!["memory", "traffic"]);
}

#[tokio::test]
async fn a_stream_authenticates_with_the_bearer_header_not_a_query_token() {
    let core = FakeController::start(handler(live_core)).await;
    let selection = Selection {
        logs: true,
        ..Selection::none()
    };
    let options = Options::new(selection).with_http_only();
    let (targets, names) = drive(&core, core.endpoint_with_secret("hunter2"), options, 1).await;
    assert_eq!(names, vec!["logs"]);
    assert_eq!(targets, vec!["/logs?level=info".to_owned()]);
    let request = core.seen().first().cloned().unwrap();
    assert_eq!(
        request.bearer(),
        Some("hunter2"),
        "the header works on every transport; `?token=` only works on a websocket upgrade"
    );
    assert!(!request.target.contains("token="), "{}", request.target);
}

// -------------------------------------------------------- 4. error decoding

#[tokio::test]
async fn a_json_error_envelope_reaches_the_caller_intact() {
    for (status, message) in [
        (404, "Resource not found"),
        (401, "Unauthorized"),
        (400, "Body invalid"),
        (400, "Must be a Selector"),
        (400, "Selector update error: proxy not exist"),
        (503, "An error occurred in the delay test"),
        (504, "get delay: all proxies timeout"),
        (500, "DNS section is disabled"),
        (500, "update error: already using latest version v1.19.31"),
        (413, "payload exceeds 1MB limit"),
    ] {
        let core = FakeController::start(handler(move |_| {
            Response::json_status(status, &format!(r#"{{"message":"{message}"}}"#))
        }))
        .await;
        match core.client().version().await.unwrap_err() {
            Error::Api {
                method,
                status: got,
                body,
                ..
            } => {
                assert_eq!(got, status);
                assert_eq!(body, message, "the message must reach the caller verbatim");
                assert_eq!(method, "GET");
            }
            other => panic!("expected an Api error for {status} {message}, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn an_absent_route_is_distinguishable_from_a_missing_object() {
    // `text/plain` 404 is Go's default: this build does not have the route.
    let core = FakeController::start(handler(|_| Response::route_absent())).await;
    match core.client().restart().await.unwrap_err() {
        Error::Unsupported(reason) => {
            assert!(reason.contains("/restart"), "{reason}");
            assert!(reason.contains("does not expose"), "{reason}");
        }
        other => panic!("expected Unsupported, got {other:?}"),
    }
    match core.client().version().await.unwrap_err() {
        Error::Api { status, body, .. } => {
            assert_eq!(status, 404);
            assert!(body.contains("not present"), "{body}");
        }
        other => panic!("expected an Api error, got {other:?}"),
    }

    // A JSON 404 is an API-level miss: the route exists, the object does not.
    let core = FakeController::start(handler(|_| {
        Response::json_status(404, r#"{"message":"Resource not found"}"#)
    }))
    .await;
    match core.client().proxy("ghost").await.unwrap_err() {
        Error::Api { status, body, .. } => {
            assert_eq!(status, 404);
            assert_eq!(body, "Resource not found");
        }
        other => panic!("expected an Api error, got {other:?}"),
    }
}

#[tokio::test]
async fn a_405_with_an_empty_body_is_reported_not_a_panic() {
    let core = FakeController::start(handler(|_| Response::method_not_allowed())).await;
    match core.client().version().await.unwrap_err() {
        Error::Api { status, body, .. } => {
            assert_eq!(status, 405);
            assert!(body.is_empty(), "{body}");
        }
        other => panic!("expected a 405 Api error, got {other:?}"),
    }
}

#[tokio::test]
async fn a_non_json_error_keeps_its_raw_body() {
    let core = FakeController::start(handler(|_| Response::text(500, "internal explosion"))).await;
    match core.client().version().await.unwrap_err() {
        Error::Api { status, body, .. } => {
            assert_eq!(status, 500);
            assert_eq!(body, "internal explosion");
        }
        other => panic!("expected an Api error, got {other:?}"),
    }
}

#[tokio::test]
async fn a_failed_delay_surfaces_as_an_error_and_never_as_a_zero_delay() {
    let core = FakeController::start(handler(|_| {
        Response::json_status(503, r#"{"message":"An error occurred in the delay test"}"#)
    }))
    .await;
    match core
        .client()
        .proxy_delay("DIRECT", "http://x/204", 5000, None)
        .await
        .unwrap_err()
    {
        Error::Api { status, .. } => assert_eq!(status, 503),
        other => panic!("a failed test must not become Ok(0): {other:?}"),
    }

    // The same for a group whose members all timed out.
    let core = FakeController::start(handler(|_| {
        Response::json_status(504, r#"{"message":"get delay: all proxies timeout"}"#)
    }))
    .await;
    match core
        .client()
        .group_delay("grp", "http://x/204", 5000, None)
        .await
        .unwrap_err()
    {
        Error::Api { status, .. } => assert_eq!(status, 504),
        other => panic!("expected a 504 Api error, got {other:?}"),
    }
}

#[tokio::test]
async fn an_empty_success_body_is_an_error_for_a_typed_call() {
    let core = FakeController::start(handler(|_| Response::empty(200))).await;
    match core.client().version().await.unwrap_err() {
        Error::Api {
            status, body, path, ..
        } => {
            assert_eq!(status, 200);
            assert!(body.contains("expected a version body"), "{body}");
            assert_eq!(path, "/version");
        }
        other => panic!("expected an Api error, got {other:?}"),
    }

    // `force_gc` is status-only: `PUT /debug/gc` answers 200 with *no* body
    // (the official docs say 204; the implementation says 200, spec §2.6), so
    // an empty success must be Ok here.
    let core = FakeController::start(handler(|_| Response::empty(200))).await;
    assert!(core.client().force_gc().await.is_ok());

    // A 404 means the `/debug` subtree is not mounted: report it as unsupported.
    let core = FakeController::start(handler(|_| Response::route_absent())).await;
    match core.client().force_gc().await.unwrap_err() {
        Error::Unsupported(reason) => assert!(reason.contains("log-level: debug"), "{reason}"),
        other => panic!("expected Unsupported, got {other:?}"),
    }

    // A 405 means the route *does* exist (the docs' `POST`), which is not the
    // same as "unsupported", so the status must be preserved.
    let core = FakeController::start(handler(|_| Response::method_not_allowed())).await;
    match core.client().force_gc().await.unwrap_err() {
        Error::Api { status, .. } => assert_eq!(status, 405),
        other => panic!("expected a 405 Api error, got {other:?}"),
    }
}

#[tokio::test]
async fn restart_and_upgrade_accept_the_documented_status_body() {
    let core = FakeController::start(handler(live_core)).await;
    let client = core.client();
    client.restart().await.unwrap();
    client.upgrade_core(Some("release"), true).await.unwrap();
    client.upgrade_ui().await.unwrap();
    assert_eq!(core.count(), 3);
}

// ------------------------------------------------------------ 5. capability

#[tokio::test]
async fn probe_reports_which_optional_routes_exist() {
    // A build with rule toggling and live config writes, but no debug router
    // and no self-upgrade: the embedded-mode surface the spec describes.
    let core = FakeController::start(handler(|request: &Request| {
        match (request.method.as_str(), request.path()) {
            ("GET", "/version") => Response::json(VERSION),
            ("PATCH", "/rules/disable") => Response::method_not_allowed(),
            ("PATCH", "/configs") => Response::no_content(),
            ("PUT", "/debug/gc") => Response::route_absent(),
            ("POST", "/upgrade/geo") => Response::route_absent(),
            _ => Response::json_status(404, r#"{"message":"Resource not found"}"#),
        }
    }))
    .await;

    let caps = core.client().probe().await.unwrap();
    assert_eq!(caps.version, "1.19.31");
    assert!(caps.rules_disable, "405 proves the route family exists");
    assert!(caps.configs_write, "204 proves it");
    assert!(!caps.debug, "a plain-text 404 proves it does not");
    assert!(!caps.upgrade);
    assert_eq!(
        caps.summary(),
        vec![
            ("core version".to_owned(), "1.19.31".to_owned()),
            ("rule toggling".to_owned(), "yes".to_owned()),
            ("live config writes".to_owned(), "yes".to_owned()),
            ("debug endpoints".to_owned(), "no".to_owned()),
            ("self-upgrade".to_owned(), "no".to_owned()),
        ]
    );

    let probes: Vec<String> = core.seen().iter().map(Request::line).collect();
    assert_eq!(
        probes,
        vec![
            "GET /version",
            "PATCH /rules/disable",
            "PATCH /configs",
            "PUT /debug/gc",
            "POST /upgrade/geo",
        ],
        "probe must use the probing methods, not the mutating ones"
    );
    assert_eq!(core.seen()[1].body.len(), 0, "probing sends no body");
}

#[tokio::test]
async fn probe_reads_a_full_build_correctly() {
    let core = FakeController::start(handler(|request: &Request| {
        match (request.method.as_str(), request.path()) {
            ("GET", "/version") => Response::json(VERSION),
            // A handler that ran and refused also proves the route exists.
            ("PATCH", "/rules/disable" | "/configs") => {
                Response::json_status(400, r#"{"message":"Body invalid"}"#)
            }
            ("PUT", "/debug/gc") => Response::empty(200),
            // A JSON 404 means the router matched, so the route family is there.
            ("POST", "/upgrade/geo") => {
                Response::json_status(404, r#"{"message":"Resource not found"}"#)
            }
            _ => Response::route_absent(),
        }
    }))
    .await;

    let caps = core.client().probe().await.unwrap();
    assert!(caps.rules_disable && caps.configs_write && caps.debug && caps.upgrade);
}

#[tokio::test]
async fn probe_fails_when_the_controller_is_unreachable() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    drop(listener);
    let client =
        Client::with_timeout(Endpoint::tcp(addr, None), Duration::from_millis(500)).unwrap();
    match client.probe().await.unwrap_err() {
        Error::ControllerUnreachable { .. } => {}
        other => panic!("expected ControllerUnreachable, got {other:?}"),
    }
    assert!(!client.is_reachable().await);
}

// ------------------------------------------------------- 6. timeout bounds

#[tokio::test]
async fn delay_timeouts_outside_the_cores_int16_range_are_rejected_client_side() {
    let core = FakeController::start(handler(live_core)).await;
    let client = core.client();

    for bad in [0u32, 32_768, 65_535, u32::MAX] {
        let err = client
            .proxy_delay("DIRECT", "http://x/204", bad, None)
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                Error::InvalidValue {
                    field: "timeout",
                    ..
                }
            ),
            "{bad} ms must be rejected client-side, got {err:?}"
        );
        let err = client
            .group_delay("grp", "http://x/204", bad, None)
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                Error::InvalidValue {
                    field: "timeout",
                    ..
                }
            ),
            "{bad} ms must be rejected client-side, got {err:?}"
        );
    }
    assert_eq!(
        core.count(),
        0,
        "a request the core would answer 400 is never worth sending"
    );

    // Both ends of the accepted range do go out.
    client
        .proxy_delay("DIRECT", "http://x/204", 1, None)
        .await
        .unwrap();
    client
        .proxy_delay("DIRECT", "http://x/204", 32_767, None)
        .await
        .unwrap();
    let targets: Vec<String> = core.seen().iter().map(|r| r.target.clone()).collect();
    assert!(targets[0].ends_with("&timeout=1"), "{}", targets[0]);
    assert!(targets[1].ends_with("&timeout=32767"), "{}", targets[1]);
}

#[tokio::test]
async fn a_delay_call_gets_the_core_its_full_timeout_plus_slack() {
    // The delay endpoints raise their own deadline to the test's timeout plus a
    // fixed slack, so a response that outlives the *client's* default timeout
    // must still succeed: the core owns the deadline.
    let core = FakeController::start(handler(|request: &Request| {
        let body = if request.path().starts_with("/group/") {
            r#"{"DIRECT":0}"#
        } else {
            r#"{"delay":9}"#
        };
        Response::json(body).after(Duration::from_millis(300))
    }))
    .await;
    let client = Client::with_timeout(core.endpoint(), Duration::from_millis(50)).unwrap();
    assert_eq!(
        client
            .proxy_delay("x", "http://x", 400, None)
            .await
            .unwrap(),
        9
    );
    assert_eq!(
        client
            .group_delay("grp", "http://x", 400, None)
            .await
            .unwrap()
            .get("DIRECT"),
        Some(&0)
    );
}

#[tokio::test]
async fn an_unreachable_controller_is_reported_with_its_endpoint() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    drop(listener);
    let client = Client::with_timeout(
        Endpoint::tcp(addr.clone(), None),
        Duration::from_millis(500),
    )
    .unwrap();
    let err = client.version().await.unwrap_err();
    assert!(
        matches!(err, Error::ControllerUnreachable { .. }),
        "{err:?}"
    );
    assert!(err.to_string().contains(&addr), "{err}");
}

#[tokio::test]
async fn a_malformed_endpoint_is_refused_before_any_request() {
    assert!(Client::new(Endpoint::tcp("127.0.0.1", None)).is_err());
    assert!(Client::new(Endpoint::tcp("127.0.0.1:notaport", None)).is_err());
    assert!(Client::new(Endpoint::unix("")).is_err());
    assert!(Client::new(Endpoint::tcp("127.0.0.1:0", None)).is_ok());
}

#[tokio::test]
async fn a_204_answer_to_a_write_is_success() {
    let core = FakeController::start(handler(|_| Response::no_content())).await;
    let client = core.client();
    client.select("grp", "DIRECT").await.unwrap();
    client.clear_selection("grp").await.unwrap();
    client.close_all_connections().await.unwrap();
    client.close_connection("id").await.unwrap();
    client.update_geo().await.unwrap();
    client.flush_dns().await.unwrap();
    client.flush_fakeip().await.unwrap();
    client.upgrade_geo().await.unwrap();
    client.storage_delete("k").await.unwrap();
    assert_eq!(core.count(), 9);
}

#[tokio::test]
async fn the_client_sends_no_proxy_token_and_no_unnecessary_headers() {
    let core = FakeController::start(handler(live_core)).await;
    let client = core.client();
    client.select("grp", "DIRECT").await.unwrap();
    let request = core.only();
    assert_eq!(request.header("content-type"), Some("application/json"));
    assert_eq!(request.header("accept"), Some("application/json"));
    assert_eq!(
        request.header("user-agent"),
        Some(concat!("clash-verge-tui/", env!("CARGO_PKG_VERSION")))
    );
    assert!(!request.target.contains("token="));
    assert!(request.header("authorization").is_none());
}
