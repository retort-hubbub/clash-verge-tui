//! Probes adapter for TUI effects.

use super::Executor;
use super::diagnostics::measure_route_speed;
use cvt_core::mihomo::client::Client;
use cvt_core::settings::TestSettings;
use cvt_core::{Error, Service};
use cvt_tui::app::SpeedMode;
use cvt_tui::row::{ProbeMode, TestKind, TestResult};
use cvt_tui::{Data, Done, Event, EventSink};
use futures_util::stream::{self, StreamExt as _};
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

impl Executor {
    // -------------------------------------------------------------- testing

    /// Measure one target and report it.
    pub(super) fn spawn_test(
        &self,
        kind: TestKind,
        target: String,
        mode: ProbeMode,
        sink: &EventSink,
    ) {
        let Ok(client) = self.client() else {
            Self::emit(sink, Event::Failed("the core is not reachable".to_owned()));
            return;
        };
        let settings = self.with_service(|service| service.settings().test.clone());
        let proxy_addr = self.with_service(|service| service.proxy_addr());
        let endpoints = if mode == ProbeMode::Connect
            || !matches!(kind, TestKind::NodeLatency | TestKind::GroupLatency)
        {
            HashMap::new()
        } else {
            match self.with_service(|service| node_endpoints(service)) {
                Ok(endpoints) => endpoints,
                Err(error) => {
                    Self::emit(sink, Event::Failed(error.to_string()));
                    return;
                }
            }
        };
        let sink = sink.clone();
        let epoch = self.test_epoch.load(Ordering::SeqCst);
        let current = Arc::clone(&self.test_epoch);

        // Report as running first, so a slow test is visible while it runs.
        let _ = sink.send(Event::Data(Data::TestResult {
            mode,
            kind,
            target: target.clone(),
            result: TestResult::Running,
        }));

        tokio::spawn(async move {
            let probe = ProbeRun {
                client: &client,
                settings: &settings,
                mode,
                endpoints: &endpoints,
                proxy_addr: proxy_addr.as_deref(),
                sink: &sink,
                epoch,
                current: &current,
            };
            let (result, tested) = run_one(&probe, kind, &target).await;
            if current.load(Ordering::SeqCst) != epoch {
                // Superseded by a cancel: nobody wants this result any more.
                return;
            }
            let _ = sink.send(Event::Data(Data::TestResult {
                mode,
                kind,
                target,
                result,
            }));
            if !matches!(kind, TestKind::Unlock(_)) {
                let _ = sink.send(Event::Done(Done::NodeTestsFinished { tested }));
            }
        });
    }

    /// Run the selected route throughput backend.
    pub(super) fn spawn_route_speed(&self, mode: SpeedMode, sink: &EventSink) {
        let Some(address) = self.with_service(|service| service.proxy_addr()) else {
            Self::emit(
                sink,
                Event::Data(Data::RouteSpeed(Err(
                    "no HTTP or mixed proxy listener is deployed".to_owned(),
                ))),
            );
            return;
        };
        let home = self.with_service(|service| service.paths().home().to_path_buf());
        if mode == SpeedMode::Speedtest && crate::speedtest::find_binary(&home).is_none() {
            Self::emit(sink, Event::Data(Data::SpeedtestMissing));
            return;
        }
        let sink = sink.clone();
        tokio::spawn(async move {
            let result = measure_route_speed(&address, mode, &home).await;
            let _ = sink.send(Event::Data(Data::RouteSpeed(result)));
        });
    }

    /// Measure everything that can be measured, one at a time.
    ///
    /// Serial on purpose: a proxy provider can hold hundreds of nodes, and a
    /// burst of parallel requests to the core is how a latency sweep becomes a
    /// timeout sweep.
    pub(super) fn spawn_all_tests(&self, mode: ProbeMode, sink: &EventSink) {
        let Ok(client) = self.client() else {
            Self::emit(sink, Event::Failed("the core is not reachable".to_owned()));
            return;
        };
        let settings = self.with_service(|service| service.settings().test.clone());
        let proxy_addr = self.with_service(|service| service.proxy_addr());
        let endpoints = if mode == ProbeMode::Connect {
            HashMap::new()
        } else {
            match self.with_service(|service| node_endpoints(service)) {
                Ok(endpoints) => endpoints,
                Err(error) => {
                    Self::emit(sink, Event::Failed(error.to_string()));
                    return;
                }
            }
        };
        let sink = sink.clone();
        let epoch = self.test_epoch.load(Ordering::SeqCst);
        let current = Arc::clone(&self.test_epoch);

        tokio::spawn(async move {
            let Ok(inventory) = client.proxies().await else {
                let _ = sink.send(Event::Failed("could not read the proxy list".to_owned()));
                return;
            };
            let mut targets = BTreeSet::new();
            for view in inventory.proxies.values() {
                if view.is_group() && view.name != "GLOBAL" {
                    for member in view.members() {
                        if inventory
                            .proxies
                            .get(member)
                            .is_some_and(|node| node.id.is_some() && !node.is_group())
                        {
                            targets.insert(member.clone());
                        }
                    }
                }
            }

            let mut tested = 0;
            for target in targets {
                if current.load(Ordering::SeqCst) != epoch {
                    return;
                }
                let _ = sink.send(Event::Data(Data::TestResult {
                    mode,
                    kind: TestKind::NodeLatency,
                    target: target.clone(),
                    result: TestResult::Running,
                }));
                let probe = ProbeRun {
                    client: &client,
                    settings: &settings,
                    mode,
                    endpoints: &endpoints,
                    proxy_addr: proxy_addr.as_deref(),
                    sink: &sink,
                    epoch,
                    current: &current,
                };
                let (result, _) = run_one(&probe, TestKind::NodeLatency, &target).await;
                if current.load(Ordering::SeqCst) != epoch {
                    return;
                }
                let _ = sink.send(Event::Data(Data::TestResult {
                    mode,
                    kind: TestKind::NodeLatency,
                    target,
                    result,
                }));
                tested += 1;
            }
            let _ = sink.send(Event::Done(Done::NodeTestsFinished { tested }));
        });
    }
}

type NodeEndpoints = HashMap<String, (String, u16)>;

pub(super) fn node_endpoints(service: &Service) -> Result<NodeEndpoints, Error> {
    let text = service.paths().read(&service.paths().runtime_config())?;
    let config = cvt_core::model::config::Config::from_yaml(&text)?;
    Ok(config
        .proxies()
        .into_iter()
        .filter_map(|proxy| Some((proxy.name, (proxy.server?, proxy.port?))))
        .collect())
}

struct ProbeRun<'a> {
    client: &'a Client,
    settings: &'a TestSettings,
    mode: ProbeMode,
    endpoints: &'a NodeEndpoints,
    proxy_addr: Option<&'a str>,
    sink: &'a EventSink,
    epoch: u64,
    current: &'a AtomicU64,
}

impl ProbeRun<'_> {
    fn report(&self, name: String, delay: Option<u16>) {
        if self.current.load(Ordering::SeqCst) == self.epoch {
            let _ = self.sink.send(Event::Data(Data::NodeDelay {
                mode: self.mode,
                name,
                delay,
            }));
        }
    }

    async fn node(&self, name: &str) -> Result<u16, String> {
        match self.mode {
            ProbeMode::Connect => {
                let expected = (!self.settings.expected_status.trim().is_empty())
                    .then_some(self.settings.expected_status.as_str());
                let first = self
                    .client
                    .proxy_delay(name, &self.settings.url, self.settings.timeout_ms, expected)
                    .await;
                // The first request often pays for DNS, TLS and a cold proxy
                // connection. Report a warmed request so a batch and a later
                // single-node retest describe the same steady-state path.
                let second = self
                    .client
                    .proxy_delay(name, &self.settings.url, self.settings.timeout_ms, expected)
                    .await;
                second.or(first).map_err(|error| error.to_string())
            }
            ProbeMode::Tcp => {
                let (host, port) = self.endpoints.get(name).ok_or_else(|| {
                    format!("{name} has no server endpoint in the deployed configuration")
                })?;
                tcp_connect_ms(host, *port, self.settings.timeout_ms).await
            }
            ProbeMode::Icmp => {
                let (host, _) = self.endpoints.get(name).ok_or_else(|| {
                    format!("{name} has no server endpoint in the deployed configuration")
                })?;
                icmp_echo(host, self.settings.timeout_ms).await
            }
        }
    }
}

pub(super) async fn tcp_connect_ms(host: &str, port: u16, timeout_ms: u32) -> Result<u16, String> {
    let began = Instant::now();
    let stream = tokio::time::timeout(
        Duration::from_millis(u64::from(timeout_ms)),
        tokio::net::TcpStream::connect((host, port)),
    )
    .await
    .map_err(|_| "TCP connection timed out".to_owned())?
    .map_err(|error| error.to_string())?;
    if stream
        .local_addr()
        .ok()
        .is_some_and(|address| is_benchmark_address(address.ip()))
        || stream
            .peer_addr()
            .ok()
            .is_some_and(|address| is_benchmark_address(address.ip()))
    {
        return Err(
            "TCP path uses a local TUN/fake-IP range; use CONNECT for proxy latency".to_owned(),
        );
    }
    Ok(u16::try_from(began.elapsed().as_millis())
        .unwrap_or(u16::MAX)
        .max(1))
}

pub(super) fn is_benchmark_address(address: std::net::IpAddr) -> bool {
    matches!(address, std::net::IpAddr::V4(ip) if {
        let octets = ip.octets();
        octets[0] == 198 && (octets[1] == 18 || octets[1] == 19)
    })
}

pub(super) async fn icmp_echo(host: &str, timeout_ms: u32) -> Result<u16, String> {
    if host.starts_with('-') {
        return Err("invalid ICMP target".to_owned());
    }
    let mut command = tokio::process::Command::new("ping");
    command.kill_on_drop(true);
    #[cfg(windows)]
    command.args(["-n", "1", "-w", &timeout_ms.to_string(), host]);
    #[cfg(not(windows))]
    command.args(["-n", "-c", "1", host]).env("LC_ALL", "C");
    let output = tokio::time::timeout(
        Duration::from_millis(u64::from(timeout_ms)),
        command.output(),
    )
    .await
    .map_err(|_| "ICMP echo timed out".to_owned())?
    .map_err(|error| format!("could not run ping: {error}"))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        let detail: String = detail
            .lines()
            .next()
            .unwrap_or_default()
            .chars()
            .take(160)
            .collect();
        return Err(if detail.is_empty() {
            "ICMP echo did not answer".to_owned()
        } else {
            format!("ICMP echo failed: {detail}")
        });
    }
    parse_ping_ms(&String::from_utf8_lossy(&output.stdout))
        .ok_or_else(|| "ping did not report a round-trip time".to_owned())
}

pub(super) fn parse_ping_ms(output: &str) -> Option<u16> {
    if output.contains("time<1ms") {
        return Some(1);
    }
    let value = output
        .split("time=")
        .nth(1)?
        .split_whitespace()
        .next()?
        .trim_end_matches("ms")
        .replace(',', ".")
        .parse::<f64>()
        .ok()?;
    if !value.is_finite() || value.is_sign_negative() {
        return None;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some((value.ceil().min(f64::from(u16::MAX)) as u16).max(1))
}

/// Measure one test-screen target and publish node results as they arrive.
async fn run_one(probe: &ProbeRun<'_>, kind: TestKind, target: &str) -> (TestResult, usize) {
    match kind {
        TestKind::NodeLatency => {
            let result = probe.node(target).await;
            probe.report(target.to_owned(), result.as_ref().ok().copied());
            (
                match result {
                    Ok(delay) => TestResult::Passed(format!("{delay} ms")),
                    Err(error) => TestResult::Failed(error),
                },
                1,
            )
        }
        TestKind::GroupLatency => {
            let group = match probe.client.group(target).await {
                Ok(group) => group,
                Err(error) => return (TestResult::Failed(error.to_string()), 0),
            };
            let members = group.members().to_vec();
            let count = members.len();
            let mut results = stream::iter(members)
                .map(|name| async move {
                    let result = probe.node(&name).await;
                    (name, result)
                })
                .buffer_unordered(probe.settings.concurrency.clamp(1, 16));
            let mut answered = 0;
            while let Some((name, result)) = results.next().await {
                if probe.current.load(Ordering::SeqCst) != probe.epoch {
                    return (TestResult::Failed("test cancelled".to_owned()), 0);
                }
                if result.is_ok() {
                    answered += 1;
                }
                probe.report(name, result.ok());
            }
            (
                if answered == 0 {
                    TestResult::Failed("no member of the group answered".to_owned())
                } else {
                    TestResult::Passed(format!("{answered} members answered"))
                },
                count,
            )
        }
        TestKind::Unlock(name) => {
            let Some(address) = probe.proxy_addr else {
                return (
                    TestResult::Failed("no HTTP or mixed proxy listener is deployed".to_owned()),
                    1,
                );
            };
            let result = async {
                let proxy = reqwest::Proxy::all(format!("http://{address}"))
                    .map_err(|error| error.to_string())?;
                let client = reqwest::Client::builder()
                    .proxy(proxy)
                    .timeout(Duration::from_secs(15))
                    .build()
                    .map_err(|error| error.to_string())?;
                let item = crate::media_unlock::check_media_unlock_item(&client, name).await?;
                let detail = match item.region {
                    Some(region) => format!("{} · {region}", item.status),
                    None => item.status,
                };
                if detail.starts_with("Yes") {
                    Ok(detail)
                } else {
                    Err(detail)
                }
            }
            .await;
            (
                match result {
                    Ok(detail) => TestResult::Passed(detail),
                    Err(error) => TestResult::Failed(error),
                },
                1,
            )
        }
    }
}
