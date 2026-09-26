//! What performs the effects the interface asks for.
//!
//! The interface is a state machine that performs no I/O: it returns
//! [`Effect`] values and waits for [`Event`]s. This is where the effects
//! actually happen, over the same `cvt_core::Service` the command line uses,
//! which is what stops the two front ends from disagreeing about what an
//! operation means.
//!
//! Three rules hold for everything here:
//!
//! * **Nothing blocks the loop.** Every effect that talks to the core is
//!   spawned on the runtime and reports back through the sink. An effect
//!   answered inline does local work only — reading the profile index,
//!   generating a document — which is microseconds rather than milliseconds.
//! * **A failure is an event.** An effect that cannot be performed sends
//!   [`Event::Failed`], which the interface shows in the status line. Nothing
//!   unwinds, and nothing is swallowed: an action that does not happen says so.
//! * **The service is behind a lock, held briefly.** `Service::set_settings`
//!   needs `&mut`, and settings are the one piece of state a user can change
//!   while the interface runs. The lock is never held across an await: a
//!   spawned task is handed what it needs — usually a `Client` — and the lock
//!   is released before the spawn.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use cvt_core::enhance::pipeline::Outcome;
use cvt_core::mihomo::client::Client;
use cvt_core::mihomo::stream::{Event as StreamEvent, Options, Selection, Stream};
use cvt_core::mihomo::types::{ConfigPatch, ProxyView};
use cvt_core::profile::item::{PrfItem, ProfileType};
use cvt_core::profile::source::SubscriptionFetcher;
use cvt_core::settings::TestSettings;
use cvt_core::validate::Severity;
use cvt_core::{AppPaths, Error, Service};
use cvt_tui::app::{Preview, PreviewChange, PreviewFinding};
use cvt_tui::row::{ConnectionRow, LogRow, NodeRow, ProfileRow, RuleRow, TestKind, TestResult};
use cvt_tui::{Data, Done, Effect, Event, EventSink, Screen};

/// Performs effects against a service.
pub struct Executor {
    service: Arc<Mutex<Service>>,
    /// Bumped when the user cancels; a batch notices at the next node instead
    /// of running to the end of a list they no longer want.
    test_epoch: Arc<AtomicU64>,
    /// Whether the live stream has been started, so it starts once rather than
    /// once per refresh.
    streaming: Arc<AtomicBool>,
}

impl std::fmt::Debug for Executor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Executor").finish_non_exhaustive()
    }
}

impl Executor {
    /// Wrap the opened application.
    #[must_use]
    pub fn new(service: Service) -> Self {
        Self {
            service: Arc::new(Mutex::new(service)),
            test_epoch: Arc::new(AtomicU64::new(0)),
            streaming: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Build the effect executor [`cvt_tui::run`] wants.
    ///
    /// A closure rather than a public `perform`, so the loop's signature stays
    /// the only contract between the two crates.
    pub fn into_executor(
        self,
    ) -> impl Fn(Effect, EventSink) -> cvt_tui::EffectFuture + Send + Sync + 'static {
        let this = Arc::new(self);
        move |effect, sink| {
            let this = Arc::clone(&this);
            Box::pin(async move {
                this.perform(effect, &sink);
            })
        }
    }

    /// Run something against the service, holding the lock for that call only.
    ///
    /// The lock is never held across an `await`. Everything here either reads
    /// or does local work that completes before it returns; the one operation
    /// that must be awaited while holding the service runs on a blocking
    /// thread, where blocking is what the thread is for.
    ///
    /// A poisoned lock is recovered from rather than propagated: the interface
    /// has no way to report "the lock is poisoned" that is more useful than
    /// continuing with the state that is in it.
    fn with_service<T>(&self, f: impl FnOnce(&mut Service) -> T) -> T {
        let mut guard: MutexGuard<'_, Service> = self
            .service
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&mut guard)
    }

    /// A client for the running core, if there is one.
    fn client(&self) -> Result<Client, Error> {
        self.with_service(|service| service.client())
    }

    /// Send an event, ignoring the report when the interface has gone.
    fn emit(sink: &EventSink, event: Event) {
        let _ = sink.send(event);
    }

    /// Start work that needs the core, and report what it produced.
    ///
    /// The client is taken before the spawn, so the lock is not held while the
    /// request is in flight and a slow request cannot stall anything else.
    fn spawn_net<F, Fut, T, G>(&self, sink: &EventSink, work: F, finish: G)
    where
        F: FnOnce(Client) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, Error>> + Send,
        T: Send + 'static,
        G: FnOnce(T) -> Event + Send + 'static,
    {
        let client = match self.client() {
            Ok(client) => client,
            Err(error) => {
                Self::emit(sink, Event::Failed(error.short()));
                return;
            }
        };
        let sink = sink.clone();
        tokio::spawn(async move {
            match work(client).await {
                Ok(value) => {
                    let _ = sink.send(finish(value));
                }
                Err(error) => {
                    tracing::debug!(error = %error, "effect failed");
                    let _ = sink.send(Event::Failed(error.short()));
                }
            }
        });
    }

    // ------------------------------------------------------------- dispatch

    /// Do one effect.
    fn perform(&self, effect: Effect, sink: &EventSink) {
        match effect {
            // The loop stops on `App::is_quit`; there is nothing to perform.
            Effect::Quit => {}

            // ---- refresh, which fans out per screen
            Effect::Refresh(screen) => self.refresh(screen, sink),

            // ---- everything that talks to the core is spawned
            Effect::SelectNode { group, member } => self.spawn_net(
                sink,
                move |client| async move { client.select(&group, &member).await },
                |()| Event::Data(Data::Notice("node selected".to_owned())),
            ),
            Effect::ClearNodePin { group } => self.spawn_net(
                sink,
                move |client| async move { client.clear_selection(&group).await },
                |()| Event::Data(Data::Notice("selection cleared".to_owned())),
            ),
            Effect::CloseConnection { id } => self.spawn_net(
                sink,
                move |client| async move { client.close_connection(&id).await },
                |()| Event::Done(Done::ConnectionClosed),
            ),
            Effect::CloseAllConnections => self.spawn_net(
                sink,
                |client| async move {
                    let before = client
                        .connections()
                        .await?
                        .connections
                        .unwrap_or_default()
                        .len();
                    client.close_all_connections().await?;
                    Ok(before)
                },
                |count| Event::Done(Done::ConnectionsClosed { count }),
            ),
            Effect::ToggleRule { index, disabled } => self.spawn_net(
                sink,
                move |client| async move {
                    let changes: BTreeMap<u32, bool> = std::iter::once((index, disabled)).collect();
                    client.set_rules_disabled(&changes).await
                },
                move |()| Event::Done(Done::RuleToggled { index, disabled }),
            ),
            Effect::UpdateRuleProviders { names } => self.spawn_net(
                sink,
                move |client| async move {
                    let mut updated = 0;
                    for name in &names {
                        client.update_rule_provider(name).await?;
                        updated += 1;
                    }
                    Ok(updated)
                },
                |count| Event::Done(Done::RuleProvidersUpdated { count }),
            ),
            Effect::SetCoreLogLevel(level) => self.spawn_net(
                sink,
                move |client| async move {
                    let patch = ConfigPatch {
                        log_level: Some(level.as_str().to_owned()),
                        ..ConfigPatch::default()
                    };
                    client.patch_configs(&patch).await
                },
                move |()| {
                    Event::Data(Data::Notice(format!(
                        "core log level is {}",
                        level.as_str()
                    )))
                },
            ),
            Effect::UpgradeCore => self.spawn_net(
                sink,
                |client| async move { client.upgrade_core(None, false).await },
                |()| Event::Done(Done::CoreUpgraded),
            ),
            Effect::UpdateGeo => self.spawn_net(
                sink,
                |client| async move { client.upgrade_geo().await },
                |()| Event::Done(Done::GeoUpdated),
            ),
            Effect::FlushCaches => self.spawn_net(
                sink,
                |client| async move {
                    // Best effort for the first: a core built without a fake-IP
                    // pool has nothing to flush and says so.
                    let _ = client.flush_fakeip().await;
                    client.flush_dns().await
                },
                |()| Event::Data(Data::Notice("caches flushed".to_owned())),
            ),

            // ---- latency tests, which serialise themselves
            Effect::TestNode { name } => self.spawn_test(TestKind::NodeLatency, name, sink),
            Effect::TestGroup { group } => self.spawn_test(TestKind::GroupLatency, group, sink),
            Effect::RunTest { kind, target } => self.spawn_test(kind, target, sink),
            Effect::TestAllNodes => self.spawn_all_tests(sink),
            Effect::CancelTests => {
                self.test_epoch.fetch_add(1, Ordering::SeqCst);
                Self::emit(
                    sink,
                    Event::Data(Data::Notice("test batch cancelled".to_owned())),
                );
            }
            Effect::ClearTestResults => {
                Self::emit(sink, Event::Done(Done::NodeTestsFinished { tested: 0 }));
            }

            // ---- updating subscriptions owns its own task
            Effect::UpdateProfiles { uids } => {
                let service = Arc::clone(&self.service);
                let sink = sink.clone();
                tokio::spawn(async move { update_profiles(service, uids, &sink).await });
            }

            // ---- the editor, which the loop awaits on purpose
            Effect::OpenEditor { path } => match open_editor(&path) {
                Ok(()) => Self::emit(
                    sink,
                    Event::Data(Data::Notice(format!("edited {}", path.display()))),
                ),
                Err(error) => Self::emit(sink, Event::Failed(error.short())),
            },
            Effect::EditProfile { uid } => match self.profile_path(&uid) {
                Ok(path) => match open_editor(&path) {
                    Ok(()) => Self::emit(sink, Event::Data(Data::Notice(format!("edited {uid}")))),
                    Err(error) => Self::emit(sink, Event::Failed(error.short())),
                },
                Err(error) => Self::emit(sink, Event::Failed(error.short())),
            },

            Effect::ExportLogs { path, contents } => match std::fs::write(&path, contents) {
                Ok(()) => Self::emit(
                    sink,
                    Event::Data(Data::Notice(format!("log written to {}", path.display()))),
                ),
                Err(error) => Self::emit(sink, Event::Failed(Error::io(&path, error).short())),
            },

            // ---- applying runs on a blocking thread, and reports once
            Effect::ApplyConfig { mode } => {
                let service = Arc::clone(&self.service);
                let sink = sink.clone();
                tokio::task::spawn_blocking(move || {
                    let guard = service
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let runtime = tokio::runtime::Handle::current();
                    let outcome = match guard.generate() {
                        Ok(outcome) => outcome,
                        Err(error) => {
                            let _ = sink.send(Event::Failed(error.short()));
                            return;
                        }
                    };
                    let changed = outcome.diff.entries.len();
                    if let Err(error) = guard.pipeline().commit(&outcome, false) {
                        let _ = sink.send(Event::Failed(error.short()));
                        return;
                    }
                    // Waiting for the core to come back can take ten seconds.
                    // This thread exists to wait; the interface does not.
                    match runtime.block_on(guard.reload(mode)) {
                        Ok(reload) => {
                            let _ = sink.send(Event::Done(Done::ConfigApplied {
                                reload: Some(reload),
                                changed,
                            }));
                        }
                        Err(error) => {
                            let _ = sink.send(Event::Failed(error.short()));
                        }
                    }
                });
            }

            // ---- everything else is local and answers immediately
            other => {
                let event = match self.local(other, sink) {
                    Ok(event) => event,
                    Err(error) => Event::Failed(error.short()),
                };
                Self::emit(sink, event);
            }
        }
    }

    /// The path of a profile's own document.
    fn profile_path(&self, uid: &str) -> Result<std::path::PathBuf, Error> {
        self.with_service(|service| {
            let store = service.store()?;
            let item = store.get(uid).ok_or_else(|| Error::ProfileNotFound {
                uid: uid.to_owned(),
            })?;
            Ok(store.paths().profiles_dir().join(item.file_name()))
        })
    }

    /// Effects that only touch local state.
    fn local(&self, effect: Effect, _sink: &EventSink) -> Result<Event, Error> {
        match effect {
            Effect::LoadProfiles => self.with_service(|service| {
                let store = service.store()?;
                Ok(Event::Data(Data::Profiles(ProfileRow::all(&store))))
            }),
            Effect::SwitchProfile { uid } => self.with_service(|service| {
                let mut store = service.store()?;
                let name = store
                    .get(&uid)
                    .map(|item| item.name.clone())
                    .ok_or_else(|| Error::ProfileNotFound { uid: uid.clone() })?;
                store.set_current(&uid)?;
                store.save()?;
                Ok(Event::Done(Done::ProfileSwitched { name }))
            }),
            Effect::SetChain { uids } => self.with_service(|service| {
                let mut store = service.store()?;
                store.set_chain(&uids)?;
                store.save()?;
                Ok(Event::Done(Done::ChainSaved))
            }),
            Effect::DeleteProfile { uid } => self.with_service(|service| {
                let mut store = service.store()?;
                let removed = store.remove(&uid)?;
                store.save()?;
                let name = removed.map_or_else(|| uid.clone(), |item| item.name);
                Ok(Event::Done(Done::ProfileDeleted { name }))
            }),
            Effect::RenameProfile { uid, name } => self.with_service(|service| {
                let mut store = service.store()?;
                store.rename(&uid, &name)?;
                store.save()?;
                Ok(Event::Done(Done::ProfileRenamed { name }))
            }),
            Effect::NewProfile { name, url } => self.with_service(|service| {
                let mut store = service.store()?;
                let remote = url.as_ref().is_some_and(|url| !url.trim().is_empty());
                let kind = if remote {
                    ProfileType::Remote
                } else {
                    ProfileType::Local
                };
                let item = match url {
                    Some(url) if remote => PrfItem::remote(store.generate_uid(kind), &name, &url),
                    _ => PrfItem::local(store.generate_uid(kind), &name),
                };
                let uid = item.uid.clone();
                store.add(item);
                // A local profile starts empty, and an empty document is not a
                // configuration: seed it so the editor opens something both the
                // validator and the core accept.
                if let Some(item) = store.get(&uid).cloned()
                    && item.kind == ProfileType::Local
                {
                    store.write_document(&item, "mode: rule\nrules:\n  - MATCH,DIRECT\n")?;
                }
                store.save()?;
                Ok(Event::Done(Done::ProfileCreated { name }))
            }),
            Effect::DetectImportSources => Ok(Event::Data(Data::ImportSources(
                AppPaths::detect_verge_homes(),
            ))),
            Effect::ImportProfiles { source } => self.with_service(|service| {
                let mut store = service.store()?;
                let report = store.import_from(&source)?;
                store.save()?;
                Ok(Event::Done(Done::ProfilesImported {
                    count: report.imported,
                }))
            }),
            Effect::PreviewConfig => self.with_service(|service| {
                let outcome = service.generate()?;
                Ok(Event::Data(Data::Preview(Box::new(preview_of(&outcome)))))
            }),
            Effect::RollbackConfig => self.with_service(|service| {
                let snapshot = service.pipeline().rollback()?;
                Ok(Event::Done(Done::ConfigRolledBack { snapshot }))
            }),
            Effect::StartCore => self.with_service(|service| {
                let pid = service.start_core()?;
                Ok(Event::Done(Done::CoreStarted { pid }))
            }),
            Effect::StopCore => self.with_service(|service| {
                service.stop_core()?;
                Ok(Event::Done(Done::CoreStopped))
            }),
            Effect::RestartCore => self.with_service(|service| {
                let pid = service.restart_core()?;
                Ok(Event::Done(Done::CoreRestarted { pid }))
            }),
            Effect::SaveSettings { settings } => self.with_service(|service| {
                service.set_settings(settings);
                service.save_settings()?;
                Ok(Event::Data(Data::Notice("settings saved".to_owned())))
            }),
            other => Err(Error::Unsupported(format!(
                "the interactive interface does not perform {other:?} yet"
            ))),
        }
    }

    // -------------------------------------------------------------- refresh

    /// Read whatever a screen shows.
    fn refresh(&self, screen: Screen, sink: &EventSink) {
        match screen {
            Screen::Home => {
                self.start_streaming(sink);
                let status = self.with_service(|service| service.core_status());
                Self::emit(sink, Event::Data(Data::Core(status)));
                self.spawn_net(
                    sink,
                    |client| async move { client.version().await },
                    |version| Event::Data(Data::Version(version.trimmed().to_owned())),
                );
            }
            Screen::Logs => self.start_streaming(sink),
            Screen::Profiles => {
                let event = self
                    .local(Effect::LoadProfiles, sink)
                    .unwrap_or_else(|error| Event::Failed(error.short()));
                Self::emit(sink, event);
            }
            Screen::Settings => {
                let settings = self.with_service(|service| service.settings().clone());
                Self::emit(sink, Event::Data(Data::Settings(Box::new(settings))));
            }
            Screen::Proxies => self.spawn_net(
                sink,
                |client| async move { client.proxies().await },
                |inventory| Event::Data(Data::Nodes(node_rows(&inventory.proxies))),
            ),
            Screen::Connections => self.spawn_net(
                sink,
                |client| async move { client.connections().await },
                |response| {
                    let rows = response
                        .connections
                        .unwrap_or_default()
                        .iter()
                        .map(ConnectionRow::from_connection)
                        .collect();
                    Event::Data(Data::Connections(rows))
                },
            ),
            Screen::Rules => {
                let providers_sink = sink.clone();
                self.spawn_net(
                    sink,
                    |client| async move { client.rules().await },
                    |rules| {
                        Event::Data(Data::Rules(rules.iter().map(RuleRow::from_rule).collect()))
                    },
                );
                self.spawn_net(
                    &providers_sink,
                    |client| async move { client.rule_providers().await },
                    |providers| {
                        Event::Data(Data::RuleProviders(
                            providers.providers.into_keys().collect(),
                        ))
                    },
                );
            }
            // The tests screen is fed by its own results; help shows nothing.
            Screen::Tests | Screen::Help => {}
        }
    }

    // ---------------------------------------------------------- live stream

    /// Start the live stream, once.
    ///
    /// Traffic, memory and logs arrive on one connection and keep arriving, so
    /// this starts on the first refresh and is then left alone: starting it
    /// again on every tick would open a new WebSocket every refresh interval.
    fn start_streaming(&self, sink: &EventSink) {
        if self.streaming.swap(true, Ordering::SeqCst) {
            return;
        }
        let level = self.with_service(|service| service.settings().ui.log_level);
        let endpoint = self.with_service(|service| service.endpoint().ok().flatten());
        let Some(endpoint) = endpoint else {
            // Not worth a status line: the interface works without a running
            // core, it simply has nothing live to show.
            tracing::debug!("no core endpoint yet, so no live stream");
            return;
        };

        let sink = sink.clone();
        tokio::spawn(async move {
            let options = Options::new(Selection {
                traffic: true,
                memory: true,
                logs: true,
                connections: false,
            })
            .with_log_level(level);

            let mut stream = match Stream::spawn(endpoint, options) {
                Ok(stream) => stream,
                Err(error) => {
                    let _ = sink.send(Event::Failed(error.short()));
                    return;
                }
            };
            while let Some(event) = stream.recv().await {
                let event = match event {
                    StreamEvent::Traffic(traffic) => Event::Data(Data::Traffic(traffic)),
                    StreamEvent::Memory(memory) => Event::Data(Data::Memory(memory.inuse)),
                    StreamEvent::Log(log) => Event::Data(Data::Log(LogRow::from_event(&log))),
                    StreamEvent::Connections(response) => {
                        let rows = response
                            .connections
                            .unwrap_or_default()
                            .iter()
                            .map(ConnectionRow::from_connection)
                            .collect();
                        Event::Data(Data::Connections(rows))
                    }
                    StreamEvent::Dropped { count } => {
                        Event::Data(Data::Notice(format!("{count} events dropped")))
                    }
                    StreamEvent::Opened { .. } | StreamEvent::Closed { .. } => continue,
                };
                // A closed sink means the interface is gone, and so is the
                // reason to keep this connection open.
                if !sink.send(event) {
                    break;
                }
            }
        });
    }

    // -------------------------------------------------------------- testing

    /// Measure one target and report it.
    fn spawn_test(&self, kind: TestKind, target: String, sink: &EventSink) {
        let Ok(client) = self.client() else {
            Self::emit(sink, Event::Failed("the core is not reachable".to_owned()));
            return;
        };
        let settings = self.with_service(|service| service.settings().test.clone());
        let sink = sink.clone();
        let epoch = self.test_epoch.load(Ordering::SeqCst);
        let current = Arc::clone(&self.test_epoch);

        // Report as running first, so a slow test is visible while it runs.
        let _ = sink.send(Event::Data(Data::TestResult {
            kind,
            target: target.clone(),
            result: TestResult::Running,
        }));

        tokio::spawn(async move {
            let result = run_one(&client, &settings, kind, &target).await;
            if current.load(Ordering::SeqCst) != epoch {
                // Superseded by a cancel: nobody wants this result any more.
                return;
            }
            let _ = sink.send(Event::Data(Data::TestResult {
                kind,
                target,
                result,
            }));
            let _ = sink.send(Event::Done(Done::NodeTestsFinished { tested: 1 }));
        });
    }

    /// Measure everything that can be measured, one at a time.
    ///
    /// Serial on purpose: a proxy provider can hold hundreds of nodes, and a
    /// burst of parallel requests to the core is how a latency sweep becomes a
    /// timeout sweep.
    fn spawn_all_tests(&self, sink: &EventSink) {
        let Ok(client) = self.client() else {
            Self::emit(sink, Event::Failed("the core is not reachable".to_owned()));
            return;
        };
        let settings = self.with_service(|service| service.settings().test.clone());
        let sink = sink.clone();
        let epoch = self.test_epoch.load(Ordering::SeqCst);
        let current = Arc::clone(&self.test_epoch);

        tokio::spawn(async move {
            let Ok(inventory) = client.proxies().await else {
                let _ = sink.send(Event::Failed("could not read the proxy list".to_owned()));
                return;
            };
            let mut targets: Vec<(TestKind, String)> = Vec::new();
            for view in inventory.proxies.values() {
                if view.is_group() {
                    targets.push((TestKind::GroupLatency, view.name.clone()));
                } else if view.id.is_some() {
                    targets.push((TestKind::NodeLatency, view.name.clone()));
                }
            }

            let mut tested = 0;
            for (kind, target) in targets {
                if current.load(Ordering::SeqCst) != epoch {
                    return;
                }
                let _ = sink.send(Event::Data(Data::TestResult {
                    kind,
                    target: target.clone(),
                    result: TestResult::Running,
                }));
                let result = run_one(&client, &settings, kind, &target).await;
                let _ = sink.send(Event::Data(Data::TestResult {
                    kind,
                    target,
                    result,
                }));
                tested += 1;
            }
            let _ = sink.send(Event::Done(Done::NodeTestsFinished { tested }));
        });
    }
}

/// Fresh copies of the given profiles, or of every remote one when the list is
/// empty.
///
/// The fetcher is built with the core's own address, which is what lets an
/// update succeed on a machine whose only route out is the core itself.
async fn update_profiles(service: Arc<Mutex<Service>>, uids: Vec<String>, sink: &EventSink) {
    let (proxy, mut store, targets) = {
        let guard = service
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let proxy = guard
            .endpoint()
            .ok()
            .flatten()
            .map(|endpoint| endpoint.describe());
        let Ok(store) = guard.store() else {
            let _ = sink.send(Event::Failed("could not read the profile index".to_owned()));
            return;
        };
        let targets: Vec<String> = if uids.is_empty() {
            store
                .items()
                .iter()
                .filter(|item| item.kind == ProfileType::Remote)
                .map(|item| item.uid.clone())
                .collect()
        } else {
            uids
        };
        // Dropped before the first await below, so the lock is not held across
        // a network request.
        drop(guard);
        (proxy, store, targets)
    };

    let fetcher = match SubscriptionFetcher::new(proxy) {
        Ok(fetcher) => fetcher,
        Err(error) => {
            let _ = sink.send(Event::Failed(error.short()));
            return;
        }
    };

    let (mut updated, mut failed) = (0usize, 0usize);
    for uid in targets {
        match fetcher.update(&mut store, &uid).await {
            Ok(_) => updated += 1,
            Err(error) => {
                tracing::debug!(profile = %uid, error = %error, "update failed");
                failed += 1;
            }
        }
    }
    if let Err(error) = store.save() {
        let _ = sink.send(Event::Failed(error.short()));
        return;
    }
    let _ = sink.send(Event::Done(Done::ProfilesUpdated { updated, failed }));
}

/// Measure one target.
async fn run_one(
    client: &Client,
    settings: &TestSettings,
    kind: TestKind,
    target: &str,
) -> TestResult {
    let expected = if settings.expected_status.trim().is_empty() {
        None
    } else {
        Some(settings.expected_status.as_str())
    };

    match kind {
        TestKind::NodeLatency => {
            match client
                .proxy_delay(target, &settings.url, settings.timeout_ms, expected)
                .await
            {
                Ok(delay) => TestResult::Passed(format!("{delay} ms")),
                Err(error) => TestResult::Failed(error.short()),
            }
        }
        TestKind::GroupLatency => {
            match client
                .group_delay(target, &settings.url, settings.timeout_ms, expected)
                .await
            {
                // A group answers with one delay per member that replied; a
                // member that did not is simply absent, which is not an error.
                Ok(delays) if delays.is_empty() => {
                    TestResult::Failed("no member of the group answered".to_owned())
                }
                Ok(delays) => TestResult::Passed(format!("{} members answered", delays.len())),
                Err(error) => TestResult::Failed(error.short()),
            }
        }
        TestKind::CoreHealth => match client.version().await {
            Ok(version) => TestResult::Passed(version.trimmed().to_owned()),
            Err(error) => TestResult::Failed(error.short()),
        },
        TestKind::DnsLookup => match client.dns_query(target, "A").await {
            Ok(answer) => {
                TestResult::Passed(format!("answer of {} bytes", answer.to_string().len()))
            }
            Err(error) => TestResult::Failed(error.short()),
        },
    }
}

/// Flatten the proxy inventory into rows: the groups first, then the nodes.
///
/// Groups come first because they are what a user chooses between; the nodes
/// are what is inside them.
fn node_rows(proxies: &BTreeMap<String, ProxyView>) -> Vec<NodeRow> {
    let mut rows = Vec::new();
    for view in proxies.values().filter(|view| view.is_group()) {
        rows.push(NodeRow::from_group(view));
    }
    for view in proxies.values().filter(|view| !view.is_group()) {
        rows.push(NodeRow::from_member(view, "", false));
    }
    rows
}

/// Summarise a generated document for the preview screen.
fn preview_of(outcome: &Outcome) -> Preview {
    Preview {
        summary: outcome.summary(),
        changes: outcome
            .diff
            .entries
            .iter()
            .map(|entry| PreviewChange {
                verb: entry.change.verb().chars().next().unwrap_or('?'),
                path: entry.path.clone(),
            })
            .collect(),
        findings: outcome
            .report
            .diagnostics
            .iter()
            .map(|diagnostic| PreviewFinding {
                tag: match diagnostic.severity {
                    Severity::Error => 'E',
                    Severity::Warning => 'W',
                    Severity::Info => 'I',
                },
                message: diagnostic.message.clone(),
                location: diagnostic.location.clone(),
                hint: diagnostic.hint.clone(),
            })
            .collect(),
        warnings: outcome.warnings.clone(),
        applicable: outcome.is_applicable(),
        truncated: outcome.diff.truncated,
    }
}

/// Open `$EDITOR` on a path, waiting for it to exit.
fn open_editor(path: &std::path::Path) -> Result<(), Error> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_owned());

    let status = std::process::Command::new(&editor)
        .arg(path)
        .status()
        .map_err(|e| Error::io(path, e))?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::ProcessFailed {
            program: editor,
            status: status.to_string(),
            stderr: String::new(),
        })
    }
}
