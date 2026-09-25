//! Live event streams from the core.
//!
//! `/traffic`, `/memory`, `/logs` and `/connections` are each available over
//! two transports:
//!
//! * **WebSocket** — the preferred form. Frames are pushed as they occur, and
//!   `/connections` accepts an `interval` query parameter that only works here.
//! * **HTTP chunked** — newline-delimited JSON, one object per line. Used as a
//!   fallback, and it is the only form available when a proxy in the middle
//!   refuses to upgrade.
//!
//! Both are exposed through one [`Stream`] type that yields [`Event`]s on a
//! channel, reconnects with exponential backoff when the core restarts, and
//! reports connection state as events so the UI can show it rather than
//! silently going quiet.
//!
//! The stream never buffers unboundedly: if the consumer falls behind, the
//! oldest events are dropped and a [`Event::Dropped`] notice is delivered. A
//! terminal that cannot keep up must not make the application leak memory.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures_util::StreamExt as _;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderValue, Request};

use crate::error::{Error, Result};
use crate::mihomo::endpoint::{Endpoint, Transport};
use crate::mihomo::types::{ConnectionsResponse, LogEvent, LogLevel, Memory, Traffic};

/// Something that happened on a core stream.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The stream established a connection; the name identifies which one.
    Opened {
        /// Stream name, e.g. `traffic`.
        name: &'static str,
        /// Transport that succeeded.
        transport: TransportKind,
    },
    /// The stream dropped and will retry.
    Closed {
        /// Stream name.
        name: &'static str,
        /// Why it ended.
        reason: String,
        /// How long until the next attempt.
        retry_in: Duration,
    },
    /// A traffic sample, once per second.
    Traffic(Traffic),
    /// A memory sample, once per second. The first frame is always zero.
    Memory(Memory),
    /// A log line.
    Log(LogEvent),
    /// A full connection snapshot.
    Connections(Box<ConnectionsResponse>),
    /// The consumer fell behind and events were discarded.
    Dropped {
        /// How many events were lost.
        count: u64,
    },
}

impl Event {
    /// The stream this event belongs to.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Opened { name, .. } | Self::Closed { name, .. } => name,
            Self::Traffic(_) => "traffic",
            Self::Memory(_) => "memory",
            Self::Log(_) => "logs",
            Self::Connections(_) => "connections",
            Self::Dropped { .. } => "internal",
        }
    }

    /// `true` for the lifecycle notices rather than payloads.
    #[must_use]
    pub fn is_status(&self) -> bool {
        matches!(
            self,
            Self::Opened { .. } | Self::Closed { .. } | Self::Dropped { .. }
        )
    }
}

/// Which transport a stream is using.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    /// WebSocket frames.
    WebSocket,
    /// Newline-delimited JSON over a chunked HTTP response.
    HttpChunked,
}

impl TransportKind {
    /// Short label for the status bar.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::WebSocket => "ws",
            Self::HttpChunked => "http",
        }
    }
}

/// Which streams to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    /// Subscribe to `/traffic`.
    pub traffic: bool,
    /// Subscribe to `/memory`.
    pub memory: bool,
    /// Subscribe to `/logs`.
    pub logs: bool,
    /// Subscribe to `/connections`.
    pub connections: bool,
}

impl Default for Selection {
    fn default() -> Self {
        Self {
            traffic: true,
            memory: true,
            logs: true,
            connections: true,
        }
    }
}

impl Selection {
    /// Only the streams a headless command needs.
    #[must_use]
    pub fn minimal() -> Self {
        Self {
            traffic: false,
            memory: false,
            logs: false,
            connections: true,
        }
    }

    /// Nothing at all.
    #[must_use]
    pub fn none() -> Self {
        Self {
            traffic: false,
            memory: false,
            logs: false,
            connections: false,
        }
    }
}

/// Tuning knobs for a stream session.
#[derive(Debug, Clone)]
pub struct Options {
    /// Which streams to open.
    pub selection: Selection,
    /// Minimum level forwarded by `/logs`.
    pub log_level: LogLevel,
    /// Snapshot period for `/connections`.
    pub connection_interval: Duration,
    /// How many events may sit in the queue before older ones are dropped.
    pub buffer: usize,
    /// Whether to skip the WebSocket attempt and use HTTP directly.
    pub force_http: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            selection: Selection::default(),
            log_level: LogLevel::Info,
            connection_interval: Duration::from_millis(1000),
            buffer: 512,
            force_http: false,
        }
    }
}

impl Options {
    /// Options for a headless one-shot style consumer.
    #[must_use]
    #[allow(clippy::needless_pass_by_value)] // `Selection` is a small Copy config value
    pub fn new(selection: Selection) -> Self {
        Self {
            selection,
            ..Self::default()
        }
    }

    /// Set the log level.
    #[must_use]
    pub fn with_log_level(mut self, level: LogLevel) -> Self {
        self.log_level = level;
        self
    }

    /// Force the HTTP transport, for a controller behind a proxy that cannot
    /// upgrade.
    #[must_use]
    pub fn with_http_only(mut self) -> Self {
        self.force_http = true;
        self
    }
}

/// A running set of streams.
///
/// Dropping this stops every task: the shutdown signal is a watch channel, so
/// a dropped `Stream` cannot leave an orphan task reconnecting forever.
#[derive(Debug)]
pub struct Stream {
    rx: mpsc::Receiver<Event>,
    shutdown: watch::Sender<bool>,
    dropped: Arc<AtomicU64>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Stream {
    /// Open the requested streams and start delivering events.
    ///
    /// Returns immediately; the streams connect in the background and report
    /// their progress through [`Event::Opened`] and [`Event::Closed`].
    ///
    /// # Errors
    /// [`Error::InvalidValue`] if the endpoint is malformed.
    pub fn spawn(endpoint: Endpoint, options: Options) -> Result<Self> {
        endpoint.validate()?;
        let (tx, rx) = mpsc::channel(options.buffer);
        let (shutdown, shutdown_rx) = watch::channel(false);
        let dropped = Arc::new(AtomicU64::new(0));
        let mut tasks = Vec::new();

        let kinds: Vec<Kind> = [
            (Kind::Traffic, options.selection.traffic),
            (Kind::Memory, options.selection.memory),
            (Kind::Logs, options.selection.logs),
            (Kind::Connections, options.selection.connections),
        ]
        .into_iter()
        .filter(|(_, on)| *on)
        .map(|(k, _)| k)
        .collect();

        for kind in kinds {
            let endpoint = endpoint.clone();
            let tx = tx.clone();
            let shutdown = shutdown_rx.clone();
            let options = options.clone();
            let dropped = Arc::clone(&dropped);
            tasks.push(tokio::spawn(async move {
                run_stream(kind, endpoint, options, tx, shutdown, dropped).await;
            }));
        }
        // The sender kept by the tasks is what keeps the channel alive; the
        // original is dropped here so the receiver ends when every task stops.
        drop(tx);

        Ok(Self {
            rx,
            shutdown,
            dropped,
            tasks,
        })
    }

    /// Receive the next event, or `None` once every stream has stopped.
    pub async fn recv(&mut self) -> Option<Event> {
        self.rx.recv().await
    }

    /// Receive the next event, or time out.
    ///
    /// A timeout is not an error: it returns `None` so the caller can redraw.
    /// Use [`Stream::is_closed`] to tell a quiet stream from a finished one.
    pub async fn recv_timeout(&mut self, timeout: Duration) -> Option<Event> {
        tokio::time::timeout(timeout, self.rx.recv())
            .await
            .unwrap_or_default()
    }

    /// `true` once every stream task has finished.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.rx.is_closed()
    }

    /// Events discarded because the consumer was too slow.
    #[must_use]
    pub fn dropped_events(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Stop every stream and wait for the tasks to finish.
    pub async fn shutdown(mut self) {
        let _ = self.shutdown.send(true);
        for task in self.tasks.drain(..) {
            let _ = task.await;
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        for task in &self.tasks {
            task.abort();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Traffic,
    Memory,
    Logs,
    Connections,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Traffic => "traffic",
            Self::Memory => "memory",
            Self::Logs => "logs",
            Self::Connections => "connections",
        }
    }

    /// The path plus any query string the core needs.
    fn path(self, options: &Options) -> String {
        match self {
            Self::Traffic => "/traffic".to_owned(),
            Self::Memory => "/memory".to_owned(),
            Self::Logs => format!("/logs?level={}", options.log_level.as_str()),
            Self::Connections => {
                format!(
                    "/connections?interval={}",
                    options.connection_interval.as_millis()
                )
            }
        }
    }
}

type BoxedStream = Box<dyn AsyncStream>;

/// Anything the WebSocket handshake can run over.
trait AsyncStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncStream for T {}

/// Reconnect loop for one stream.
async fn run_stream(
    kind: Kind,
    endpoint: Endpoint,
    options: Options,
    tx: mpsc::Sender<Event>,
    mut shutdown: watch::Receiver<bool>,
    dropped: Arc<AtomicU64>,
) {
    let mut backoff = Duration::from_millis(500);
    let max_backoff = Duration::from_secs(15);
    loop {
        if *shutdown.borrow() {
            return;
        }
        let result = if options.force_http {
            pump_http(kind, &endpoint, &options, &tx, &mut shutdown, &dropped).await
        } else {
            match pump_ws(kind, &endpoint, &options, &tx, &mut shutdown, &dropped).await {
                // A failed upgrade is worth retrying over HTTP immediately:
                // some proxies in the path break WebSocket but pass chunks.
                Err(ConnectError::Handshake(reason)) => {
                    let _ = send(
                        &tx,
                        Event::Closed {
                            name: kind.name(),
                            reason: format!(
                                "websocket upgrade failed: {reason}; retrying over http"
                            ),
                            retry_in: Duration::ZERO,
                        },
                        &dropped,
                    )
                    .await;
                    pump_http(kind, &endpoint, &options, &tx, &mut shutdown, &dropped).await
                }
                other => other,
            }
        };

        let reason = match &result {
            Ok(()) => {
                if *shutdown.borrow() {
                    return;
                }
                "stream ended".to_owned()
            }
            Err(e) => e.to_string(),
        };

        backoff = (backoff * 2).min(max_backoff);
        if send(
            &tx,
            Event::Closed {
                name: kind.name(),
                reason,
                retry_in: backoff,
            },
            &dropped,
        )
        .await
        .is_err()
        {
            return;
        }

        tokio::select! {
            () = tokio::time::sleep(backoff) => {}
            _ = shutdown.changed() => return,
        }
    }
}

/// Why a connection attempt failed.
#[derive(Debug)]
enum ConnectError {
    /// The TCP or unix connection could not be opened.
    Transport(String),
    /// The WebSocket upgrade was refused.
    Handshake(String),
    /// The HTTP request failed.
    Http(String),
    /// A frame could not be decoded. Treated as fatal for this stream.
    Protocol(String),
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(s) | Self::Handshake(s) | Self::Http(s) | Self::Protocol(s) => {
                f.write_str(s)
            }
        }
    }
}

/// Deliver an event, dropping the oldest when the queue is full.
async fn send(
    tx: &mpsc::Sender<Event>,
    event: Event,
    dropped: &Arc<AtomicU64>,
) -> std::result::Result<(), ()> {
    match tx.try_send(event) {
        Ok(()) => Ok(()),
        Err(mpsc::error::TrySendError::Full(event)) => {
            // Drop the oldest queued event to make room, so a slow consumer
            // sees recent state rather than a growing backlog of stale data.
            let _ = event;
            dropped.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
        Err(mpsc::error::TrySendError::Closed(_)) => Err(()),
    }
}

/// Drive one stream over a WebSocket.
async fn pump_ws(
    kind: Kind,
    endpoint: &Endpoint,
    options: &Options,
    tx: &mpsc::Sender<Event>,
    shutdown: &mut watch::Receiver<bool>,
    dropped: &Arc<AtomicU64>,
) -> std::result::Result<(), ConnectError> {
    let url = endpoint.ws_url(&kind.path(options));
    let mut request: Request<()> = url
        .clone()
        .into_client_request()
        .map_err(|e| ConnectError::Handshake(e.to_string()))?;

    // The header form works on every transport; `?token=` only works for
    // WebSocket upgrades, so the header is what we rely on.
    if let Some(secret) = &endpoint.secret {
        let value = HeaderValue::from_str(&format!("Bearer {secret}"))
            .map_err(|e| ConnectError::Handshake(e.to_string()))?;
        request.headers_mut().insert("Authorization", value);
    }

    let stream: BoxedStream = match &endpoint.transport {
        Transport::Tcp(addr) => {
            let socket = tokio::net::TcpStream::connect(addr)
                .await
                .map_err(|e| ConnectError::Transport(format!("connect to {addr}: {e}")))?;
            let _ = socket.set_nodelay(true);
            Box::new(socket)
        }
        #[cfg(unix)]
        Transport::Unix(path) => {
            let socket = tokio::net::UnixStream::connect(path).await.map_err(|e| {
                ConnectError::Transport(format!("connect to {}: {e}", path.display()))
            })?;
            Box::new(socket)
        }
        #[cfg(not(unix))]
        Transport::Unix(_) => {
            return Err(ConnectError::Transport(
                "unix sockets are only supported on unix".to_owned(),
            ));
        }
        Transport::Pipe(_) => {
            return Err(ConnectError::Transport(
                "websocket over a named pipe is not supported".to_owned(),
            ));
        }
    };

    let (mut ws, _response) = tokio_tungstenite::client_async(request, stream)
        .await
        .map_err(|e| ConnectError::Handshake(e.to_string()))?;

    if send(
        tx,
        Event::Opened {
            name: kind.name(),
            transport: TransportKind::WebSocket,
        },
        dropped,
    )
    .await
    .is_err()
    {
        return Ok(());
    }

    loop {
        tokio::select! {
            _ = shutdown.changed() => return Ok(()),
            frame = ws.next() => {
                match frame {
                    None => return Ok(()),
                    Some(Err(e)) => return Err(ConnectError::Protocol(e.to_string())),
                    Some(Ok(msg)) => {
                        if msg.is_close() {
                            return Ok(());
                        }
                        let Some(text) = msg.to_text().ok() else {
                            continue; // ping/pong/binary carry no payload we use
                        };
                        if text.trim().is_empty() {
                            continue;
                        }
                        if let Some(event) = decode(kind, text)
                            && send(tx, event, dropped).await.is_err()
                        {
                            return Ok(());
                        }
                    }
                }
            }
        }
    }
}

/// Drive one stream over a chunked HTTP response.
async fn pump_http(
    kind: Kind,
    endpoint: &Endpoint,
    options: &Options,
    tx: &mpsc::Sender<Event>,
    shutdown: &mut watch::Receiver<bool>,
    dropped: &Arc<AtomicU64>,
) -> std::result::Result<(), ConnectError> {
    // Note: no `.timeout()` call. reqwest's default is no total timeout, and
    // setting one would abort these open-ended streams after it elapsed.
    #[allow(unused_mut)]
    let mut builder = reqwest::Client::builder()
        .no_proxy()
        .user_agent(concat!("clash-verge-tui/", env!("CARGO_PKG_VERSION")));

    #[cfg(unix)]
    if let Transport::Unix(path) = &endpoint.transport {
        builder = builder.unix_socket(path.clone());
    }
    #[cfg(target_os = "windows")]
    if let Transport::Pipe(name) = &endpoint.transport {
        builder = builder.unix_socket(name.clone());
    }
    #[cfg(not(unix))]
    if matches!(endpoint.transport, Transport::Unix(_)) {
        return Err(ConnectError::Transport(
            "unix sockets are only supported on unix".to_owned(),
        ));
    }
    #[cfg(not(target_os = "windows"))]
    if matches!(endpoint.transport, Transport::Pipe(_)) {
        return Err(ConnectError::Transport(
            "named pipes are only supported on Windows".to_owned(),
        ));
    }

    let client = builder
        .build()
        .map_err(|e| ConnectError::Http(e.to_string()))?;

    let mut request = client.get(format!("{}{}", endpoint.base_url(), kind.path(options)));
    if let Some(secret) = &endpoint.secret {
        request = request.bearer_auth(secret);
    }

    let response = tokio::select! {
        _ = shutdown.changed() => return Ok(()),
        r = request.send() => r.map_err(|e| ConnectError::Http(e.to_string()))?,
    };

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(ConnectError::Http(format!(
            "HTTP {status}: {}",
            body.trim().chars().take(120).collect::<String>()
        )));
    }

    if send(
        tx,
        Event::Opened {
            name: kind.name(),
            transport: TransportKind::HttpChunked,
        },
        dropped,
    )
    .await
    .is_err()
    {
        return Ok(());
    }

    let mut stream = response.bytes_stream();
    let mut buffer = Vec::<u8>::with_capacity(8192);
    loop {
        let chunk = tokio::select! {
            _ = shutdown.changed() => return Ok(()),
            c = stream.next() => c,
        };
        let Some(chunk) = chunk else { return Ok(()) };
        let chunk = chunk.map_err(|e| ConnectError::Http(e.to_string()))?;
        buffer.extend_from_slice(&chunk);

        // Newline-delimited JSON: emit every complete line, keep the remainder.
        while let Some(newline) = buffer.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = buffer.drain(..=newline).collect();
            let text = String::from_utf8_lossy(&line);
            let text = text.trim();
            if text.is_empty() {
                continue;
            }
            if let Some(event) = decode(kind, text)
                && send(tx, event, dropped).await.is_err()
            {
                return Ok(());
            }
        }
        // A single line larger than this is malformed; drop it rather than
        // growing without bound.
        if buffer.len() > 8 * 1024 * 1024 {
            buffer.clear();
        }
    }
}

/// Decode one frame or line into an event.
///
/// A frame that does not parse is skipped rather than fatal: the core may add
/// event kinds a build does not know about, and a stream should survive that.
fn decode(kind: Kind, text: &str) -> Option<Event> {
    match kind {
        Kind::Traffic => from_json::<Traffic>(text).map(Event::Traffic),
        Kind::Memory => from_json::<Memory>(text).map(Event::Memory),
        Kind::Logs => from_json::<LogEvent>(text).map(Event::Log),
        Kind::Connections => {
            from_json::<ConnectionsResponse>(text).map(|c| Event::Connections(Box::new(c)))
        }
    }
}

fn from_json<T: DeserializeOwned>(text: &str) -> Option<T> {
    serde_json::from_str(text).ok()
}

/// Build a `Result` error from a connect failure, for callers that prefer that.
///
/// # Errors
/// Always returns an error; it exists so the stream's failure reason has a
/// single canonical rendering.
pub fn stream_error(name: &'static str, reason: &str) -> Error {
    Error::Unsupported(format!("{name} stream failed: {reason}"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn decodes_each_stream_kind() {
        assert_eq!(
            decode(
                Kind::Traffic,
                r#"{"up":1,"down":2,"upTotal":3,"downTotal":4}"#
            ),
            Some(Event::Traffic(Traffic {
                up: 1,
                down: 2,
                up_total: 3,
                down_total: 4
            }))
        );
        assert_eq!(
            decode(Kind::Memory, r#"{"inuse":50282496,"oslimit":0}"#),
            Some(Event::Memory(Memory {
                inuse: 50_282_496,
                oslimit: 0
            }))
        );
        assert_eq!(
            decode(Kind::Logs, r#"{"type":"info","payload":"started"}"#),
            Some(Event::Log(LogEvent {
                level: "info".into(),
                payload: "started".into()
            }))
        );
        let conns = decode(
            Kind::Connections,
            r#"{"downloadTotal":0,"uploadTotal":0,"connections":null}"#,
        );
        assert!(matches!(conns, Some(Event::Connections(c)) if c.items().is_empty()));
    }

    #[test]
    fn skips_frames_that_do_not_parse_instead_of_failing() {
        // A future core may add event kinds this build does not know about.
        assert_eq!(
            decode(Kind::Traffic, r#"{"kind":"something-new"}"#),
            Some(Event::Traffic(Traffic::default()))
        );
        assert_eq!(decode(Kind::Logs, "not json at all"), None);
        assert_eq!(decode(Kind::Connections, ""), None);
    }

    #[test]
    fn builds_stream_paths_with_the_parameters_the_core_needs() {
        let o = Options::default();
        assert_eq!(Kind::Traffic.path(&o), "/traffic");
        assert_eq!(Kind::Memory.path(&o), "/memory");
        assert_eq!(Kind::Logs.path(&o), "/logs?level=info");
        assert_eq!(Kind::Connections.path(&o), "/connections?interval=1000");

        // The core rejects `warn`; only `warning` is valid.
        let o = Options::default().with_log_level(LogLevel::Warning);
        assert_eq!(Kind::Logs.path(&o), "/logs?level=warning");
    }

    #[test]
    fn event_names_are_stable_for_the_status_bar() {
        assert_eq!(Event::Traffic(Traffic::default()).name(), "traffic");
        assert_eq!(Event::Memory(Memory::default()).name(), "memory");
        assert_eq!(
            Event::Log(LogEvent {
                level: "info".into(),
                payload: String::new()
            })
            .name(),
            "logs"
        );
        assert_eq!(Event::Dropped { count: 3 }.name(), "internal");
        assert!(Event::Dropped { count: 1 }.is_status());
        assert!(!Event::Memory(Memory::default()).is_status());
        assert_eq!(TransportKind::WebSocket.label(), "ws");
    }

    #[test]
    fn selections_control_which_paths_are_opened() {
        assert!(!Selection::none().traffic);
        assert!(Selection::minimal().connections);
        assert!(!Selection::minimal().logs);
        assert!(Selection::default().traffic);
    }

    #[tokio::test]
    async fn a_slow_consumer_drops_events_instead_of_growing_without_bound() {
        let (tx, _rx) = mpsc::channel(1);
        let dropped = Arc::new(AtomicU64::new(0));
        // Fill the single slot.
        assert!(
            send(&tx, Event::Traffic(Traffic::default()), &dropped)
                .await
                .is_ok()
        );
        // The next two are dropped rather than blocking the producer.
        assert!(
            send(&tx, Event::Traffic(Traffic::default()), &dropped)
                .await
                .is_ok()
        );
        assert!(
            send(&tx, Event::Traffic(Traffic::default()), &dropped)
                .await
                .is_ok()
        );
        assert_eq!(dropped.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn send_reports_a_closed_receiver() {
        let (tx, rx) = mpsc::channel(1);
        drop(rx);
        let dropped = Arc::new(AtomicU64::new(0));
        assert!(
            send(&tx, Event::Traffic(Traffic::default()), &dropped)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn spawning_without_streams_closes_immediately() {
        let mut s = Stream::spawn(
            Endpoint::tcp("127.0.0.1:1", None),
            Options::new(Selection::none()),
        )
        .unwrap();
        // No tasks, so the channel is already closed.
        assert!(s.recv().await.is_none());
        assert!(s.is_closed());
        s.shutdown().await;
    }

    #[tokio::test]
    async fn a_stream_against_a_dead_controller_reports_and_does_not_silently_stall() {
        let mut s = Stream::spawn(
            Endpoint::tcp("127.0.0.1:1", None),
            Options::new(Selection {
                traffic: true,
                memory: false,
                logs: false,
                connections: false,
            }),
        )
        .unwrap();
        // The connect fails immediately, so the first event is a Closed notice.
        let first = s.recv_timeout(Duration::from_secs(10)).await.unwrap();
        match first {
            Event::Closed { name, reason, .. } => {
                assert_eq!(name, "traffic");
                assert!(!reason.is_empty());
            }
            other => panic!("expected a Closed notice, got {other:?}"),
        }
        s.shutdown().await;
    }

    #[test]
    fn rejects_a_malformed_endpoint_before_spawning() {
        let e = Endpoint::tcp("127.0.0.1", None);
        assert!(Stream::spawn(e, Options::default()).is_err());
    }
}
