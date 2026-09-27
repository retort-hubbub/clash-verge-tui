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

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use cvt_core::enhance::pipeline::Outcome;
use cvt_core::mihomo::client::Client;
use cvt_core::mihomo::stream::{Event as StreamEvent, Options, Selection, Stream};
use cvt_core::mihomo::types::{ConfigPatch, ProxyView};
use cvt_core::profile::item::{PrfItem, ProfileType};
use cvt_core::profile::source::{SubscriptionFetcher, is_due};
use cvt_core::settings::TestSettings;
use cvt_core::validate::Severity;
use cvt_core::{AppPaths, Error, Service};
use cvt_tui::app::{Preview, PreviewChange, PreviewFinding};
use cvt_tui::row::{
    ConnectionRow, LogRow, NodeRow, ProbeMode, ProfileRow, RuleRow, TestKind, TestResult,
};
use cvt_tui::{Data, Done, Effect, Event, EventSink, Screen};
use futures_util::stream::{self, StreamExt as _};

/// Performs effects against a service.
pub struct Executor {
    service: Arc<Mutex<Service>>,
    /// Suppress controller reads between process launch and API readiness.
    starting: Arc<AtomicBool>,
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
            starting: Arc::new(AtomicBool::new(false)),
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
                Self::emit(sink, Event::Failed(error.to_string()));
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
                    let _ = sink.send(Event::Failed(error.to_string()));
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

            Effect::Startup => {
                let (auto_start, update_on_start) = self.with_service(|service| {
                    (
                        service.settings().core.auto_start,
                        service.settings().update.update_on_start,
                    )
                });
                if auto_start || update_on_start {
                    let service = Arc::clone(&self.service);
                    let starting = Arc::clone(&self.starting);
                    let sink = sink.clone();
                    tokio::spawn(async move {
                        if auto_start {
                            let start_service = Arc::clone(&service);
                            starting.store(true, Ordering::SeqCst);
                            let started = tokio::task::spawn_blocking(move || {
                                let guard = start_service
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                                if guard.core_status().is_running() {
                                    Ok(None)
                                } else {
                                    launch_core(&guard, false).map(Some)
                                }
                            })
                            .await;
                            let started = match started {
                                Ok(Ok(Some(launch))) => ready_mode(launch).await.map(Some),
                                Ok(Ok(None)) => Ok(None),
                                Ok(Err(error)) => Err(error),
                                Err(error) => Err(Error::Unsupported(error.to_string())),
                            };
                            starting.store(false, Ordering::SeqCst);
                            match started {
                                Ok(Some((pid, mode))) => {
                                    Self::emit(&sink, Event::Data(Data::CoreMode(mode)));
                                    Self::emit(&sink, Event::Done(Done::CoreStarted { pid }));
                                }
                                Ok(None) => {}
                                Err(error) => {
                                    Self::emit(&sink, Event::Failed(error.to_string()));
                                }
                            }
                        }
                        if update_on_start {
                            update_profiles(service, Vec::new(), &sink).await;
                        }
                    });
                }
            }

            // ---- refresh, which fans out per screen
            Effect::Refresh(screen) => self.refresh(screen, sink),
            Effect::StartCore | Effect::RestartCore => {
                let service = Arc::clone(&self.service);
                let starting = Arc::clone(&self.starting);
                let sink = sink.clone();
                let restart = matches!(effect, Effect::RestartCore);
                starting.store(true, Ordering::SeqCst);
                tokio::spawn(async move {
                    let result = tokio::task::spawn_blocking(move || {
                        let guard = service
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        launch_core(&guard, restart)
                    })
                    .await;
                    let result = match result {
                        Ok(Ok(launch)) => ready_mode(launch).await,
                        Ok(Err(error)) => Err(error),
                        Err(error) => Err(Error::Unsupported(error.to_string())),
                    };
                    starting.store(false, Ordering::SeqCst);
                    match result {
                        Ok((pid, mode)) => {
                            Self::emit(&sink, Event::Data(Data::CoreMode(mode)));
                            Self::emit(
                                &sink,
                                Event::Done(if restart {
                                    Done::CoreRestarted { pid }
                                } else {
                                    Done::CoreStarted { pid }
                                }),
                            );
                        }
                        Err(error) => Self::emit(&sink, Event::Failed(error.to_string())),
                    }
                });
            }

            // ---- everything that talks to the core is spawned
            // The two selection effects are hand-written rather than going
            // through `spawn_net`, because they touch the profile index as well
            // as the core: the choice is recorded so that the next apply, which
            // rebuilds every group, does not throw it away.
            Effect::SelectNode { group, member } => {
                let service = Arc::clone(&self.service);
                let sink = sink.clone();
                tokio::spawn(async move {
                    let client = {
                        let guard = service
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        guard.client()
                    };
                    let selected = match client {
                        Ok(client) => client.select(&group, &member).await,
                        Err(error) => Err(error),
                    };
                    match selected {
                        Ok(()) => {
                            let recorded = {
                                let guard = service
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                                guard.remember_selection(&group, &member)
                            };
                            let _ = sink.send(Event::Done(Done::NodeSelected {
                                group: group.clone(),
                                member: member.clone(),
                            }));
                            if let Err(error) = recorded {
                                let _ = sink.send(Event::Data(Data::Notice(format!(
                                    "{group}: {member} (selection was not saved: {error})"
                                ))));
                            }
                        }
                        Err(error) => {
                            let _ = sink.send(Event::Failed(error.to_string()));
                        }
                    }
                });
            }
            Effect::ClearNodePin { group } => {
                let service = Arc::clone(&self.service);
                let sink = sink.clone();
                tokio::spawn(async move {
                    let client = {
                        let guard = service
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        guard.client()
                    };
                    let cleared = match client {
                        Ok(client) => client.clear_selection(&group).await,
                        Err(error) => Err(error),
                    };
                    match cleared {
                        Ok(()) => {
                            let forgotten = {
                                let guard = service
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                                guard.forget_selection(&group)
                            };
                            let _ = sink.send(Event::Done(Done::NodeCleared {
                                group: group.clone(),
                            }));
                            if let Err(error) = forgotten {
                                let _ = sink.send(Event::Data(Data::Notice(format!(
                                    "{group}: automatic (choice was not forgotten: {error})"
                                ))));
                            }
                        }
                        Err(error) => {
                            let _ = sink.send(Event::Failed(error.to_string()));
                        }
                    }
                });
            }
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
            Effect::SetCoreMode { mode } => self.spawn_net(
                sink,
                {
                    let mode = mode.clone();
                    move |client| async move {
                        let patch = ConfigPatch {
                            mode: Some(mode),
                            ..ConfigPatch::default()
                        };
                        client.patch_configs(&patch).await
                    }
                },
                move |()| Event::Done(Done::CoreModeChanged { mode }),
            ),
            Effect::UpgradeCore => {
                let service = Arc::clone(&self.service);
                let sink = sink.clone();
                tokio::spawn(async move {
                    let (paths, was_running) = {
                        let guard = service
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        (guard.paths().clone(), guard.core_status().is_running())
                    };
                    let _ = sink.send(Event::Data(Data::Notice(
                        "downloading latest mihomo core...".to_owned(),
                    )));
                    let outcome = cvt_core::mihomo::download::install_latest_core(&paths).await;
                    match outcome {
                        Ok(version) => {
                            if was_running {
                                let restart_service = Arc::clone(&service);
                                let _ = tokio::task::spawn_blocking(move || {
                                    let guard = restart_service
                                        .lock()
                                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                                    let _ = guard.restart_core();
                                })
                                .await;
                            }
                            let status = {
                                let guard = service
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                                guard.core_status()
                            };
                            let _ = sink.send(Event::Data(Data::Core(status)));
                            let _ = sink.send(Event::Done(Done::CoreUpgraded { version }));
                        }
                        Err(error) => {
                            let _ = sink.send(Event::Failed(error.to_string()));
                        }
                    }
                });
            }
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
            Effect::TestNode { name, mode } => {
                self.spawn_test(TestKind::NodeLatency, name, mode, sink);
            }
            Effect::TestGroup { group, mode } => {
                self.spawn_test(TestKind::GroupLatency, group, mode, sink);
            }
            Effect::RunTest { kind, target, mode } => self.spawn_test(kind, target, mode, sink),
            Effect::TestAllNodes { mode } => self.spawn_all_tests(mode, sink),
            Effect::CancelTests | Effect::ClearTestResults => {
                self.test_epoch.fetch_add(1, Ordering::SeqCst);
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
                Err(error) => Self::emit(sink, Event::Failed(error.to_string())),
            },
            Effect::EditProfile { uid } => match self.profile_path(&uid) {
                Ok(path) => match open_editor(&path) {
                    Ok(()) => Self::emit(sink, Event::Data(Data::Notice(format!("edited {uid}")))),
                    Err(error) => Self::emit(sink, Event::Failed(error.to_string())),
                },
                Err(error) => Self::emit(sink, Event::Failed(error.to_string())),
            },

            Effect::ExportLogs { path, contents } => match std::fs::write(&path, contents) {
                Ok(()) => Self::emit(
                    sink,
                    Event::Data(Data::Notice(format!("log written to {}", path.display()))),
                ),
                Err(error) => Self::emit(sink, Event::Failed(Error::io(&path, error).to_string())),
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
                            let _ = sink.send(Event::Failed(error.to_string()));
                            return;
                        }
                    };
                    let changed = outcome.diff.entries.len();
                    if let Err(error) = guard.pipeline().commit(&outcome, false) {
                        let _ = sink.send(Event::Failed(error.to_string()));
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
                            let _ = sink.send(Event::Failed(error.to_string()));
                        }
                    }
                });
            }
            Effect::PrepareConfig => {
                let service = Arc::clone(&self.service);
                let sink = sink.clone();
                tokio::task::spawn_blocking(move || {
                    let guard = service
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let outcome = match guard.generate() {
                        Ok(outcome) => outcome,
                        Err(error) => {
                            let _ = sink.send(Event::Failed(error.to_string()));
                            return;
                        }
                    };
                    let changed = outcome.diff.entries.len();
                    match guard.pipeline().commit(&outcome, false) {
                        Ok(()) => {
                            let _ = sink.send(Event::Done(Done::ConfigApplied {
                                reload: None,
                                changed,
                            }));
                        }
                        Err(error) => {
                            let _ = sink.send(Event::Failed(error.to_string()));
                        }
                    }
                });
            }

            // ---- everything else is local and answers immediately
            other => {
                let event = match self.local(other, sink) {
                    Ok(event) => event,
                    Err(error) => Event::Failed(error.to_string()),
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
                if !service.core_status().is_running() {
                    let path = service.paths().runtime_config();
                    if path.exists() {
                        std::fs::remove_file(&path).map_err(|error| Error::io(&path, error))?;
                    }
                }
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
                let was_current = store.current_uid() == Some(uid.as_str());
                let removed = store.remove(&uid)?;
                store.save()?;
                if was_current {
                    let path = service.paths().runtime_config();
                    if path.exists() {
                        std::fs::remove_file(&path).map_err(|error| Error::io(&path, error))?;
                    }
                }
                let name = removed.map_or_else(|| uid.clone(), |item| item.name);
                Ok(Event::Done(Done::ProfileDeleted { name, was_current }))
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
                Ok(Event::Done(Done::ProfileCreated {
                    name,
                    uid: Some(uid),
                    is_remote: remote,
                }))
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
            Effect::StopCore => self.with_service(|service| {
                service.stop_core()?;
                Ok(Event::Done(Done::CoreStopped))
            }),
            Effect::SaveSettings { settings } => self.with_service(|service| {
                service.set_settings(settings);
                service.save_settings()?;
                // `Done::SettingsSaved`, not a notice: it is the variant that
                // clears the interface's dirty flag, and nothing produced it, so
                // the flag stayed set after a successful save. The status line
                // and the flag are two different things, and only one of them
                // was being sent.
                Ok(Event::Done(Done::SettingsSaved))
            }),
            other => Err(Error::Unsupported(format!(
                "the interactive interface does not perform {other:?} yet"
            ))),
        }
    }

    // -------------------------------------------------------------- refresh

    /// A background read only reaches a controller while the core is running.
    /// Keep a configured but stopped controller quiet.
    fn has_controller_endpoint(&self, sink: &EventSink) -> bool {
        if self.starting.load(Ordering::SeqCst) {
            return false;
        }
        match self.with_service(|service| {
            if !service.core_status().is_running() {
                return Ok(None);
            }
            service.endpoint()
        }) {
            Ok(Some(_)) => true,
            Ok(None) => false,
            Err(error) => {
                Self::emit(sink, Event::Failed(error.to_string()));
                false
            }
        }
    }

    /// Read whatever a screen shows.
    fn refresh(&self, screen: Screen, sink: &EventSink) {
        match screen {
            Screen::Home => {
                self.start_streaming(sink);
                let status = self.with_service(|service| service.core_status());
                Self::emit(sink, Event::Data(Data::Core(status)));
                if self.has_controller_endpoint(sink) {
                    self.spawn_net(
                        sink,
                        |client| async move { client.version().await },
                        |version| Event::Data(Data::Version(version.trimmed().to_owned())),
                    );
                    self.spawn_net(
                        sink,
                        |client| async move { client.configs().await },
                        |config| {
                            Event::Data(Data::CoreMode(
                                config.mode.unwrap_or_else(|| "rule".to_owned()),
                            ))
                        },
                    );
                }
            }
            Screen::Logs => self.start_streaming(sink),
            Screen::Profiles => {
                let event = self
                    .local(Effect::LoadProfiles, sink)
                    .unwrap_or_else(|error| Event::Failed(error.to_string()));
                Self::emit(sink, event);
            }
            Screen::Settings => {
                let settings = self.with_service(|service| service.settings().clone());
                Self::emit(sink, Event::Data(Data::Settings(Box::new(settings))));
            }
            Screen::Proxies => {
                if self.has_controller_endpoint(sink) {
                    self.spawn_net(
                        sink,
                        |client| async move {
                            tokio::try_join!(client.proxies(), client.configs())
                        },
                        |(inventory, config)| {
                            Event::Data(Data::Nodes(node_rows(
                                &inventory.proxies,
                                config.mode.as_deref(),
                            )))
                        },
                    );
                } else {
                    let rows = self.with_service(|service| {
                        service
                            .generate()
                            .ok()
                            .map_or_else(Vec::new, |outcome| config_node_rows(&outcome.config))
                    });
                    Self::emit(sink, Event::Data(Data::Nodes(rows)));
                }
            }
            Screen::Connections => {
                if self.has_controller_endpoint(sink) {
                    self.spawn_net(
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
                    );
                } else {
                    Self::emit(sink, Event::Data(Data::Connections(Vec::new())));
                }
            }
            Screen::Rules => {
                if !self.has_controller_endpoint(sink) {
                    Self::emit(sink, Event::Data(Data::Rules(Vec::new())));
                    Self::emit(sink, Event::Data(Data::RuleProviders(Vec::new())));
                    return;
                }
                self.spawn_net(
                    sink,
                    |client| async move { client.rules().await },
                    |rules| {
                        Event::Data(Data::Rules(rules.iter().map(RuleRow::from_rule).collect()))
                    },
                );
                self.spawn_net(
                    sink,
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
        if self.starting.load(Ordering::SeqCst) {
            return;
        }
        let (level, endpoint) = self.with_service(|service| {
            (
                service.settings().ui.log_level,
                service
                    .core_status()
                    .is_running()
                    .then(|| service.endpoint().ok().flatten())
                    .flatten(),
            )
        });
        let Some(endpoint) = endpoint else {
            // Not worth a status line: the interface works without a running
            // core, it simply has nothing live to show.
            tracing::debug!("no core endpoint yet, so no live stream");
            return;
        };
        if self.streaming.swap(true, Ordering::SeqCst) {
            return;
        }

        let sink = sink.clone();
        let streaming = Arc::clone(&self.streaming);
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
                    let _ = sink.send(Event::Failed(error.to_string()));
                    streaming.store(false, Ordering::SeqCst);
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
            streaming.store(false, Ordering::SeqCst);
        });
    }

    // -------------------------------------------------------------- testing

    /// Measure one target and report it.
    fn spawn_test(&self, kind: TestKind, target: String, mode: ProbeMode, sink: &EventSink) {
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
            let _ = sink.send(Event::Done(Done::NodeTestsFinished { tested }));
        });
    }

    /// Measure everything that can be measured, one at a time.
    ///
    /// Serial on purpose: a proxy provider can hold hundreds of nodes, and a
    /// burst of parallel requests to the core is how a latency sweep becomes a
    /// timeout sweep.
    fn spawn_all_tests(&self, mode: ProbeMode, sink: &EventSink) {
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

/// Fresh copies of the given profiles, or of every due remote one when the
/// list is empty.
///
/// The fetcher is built with the core's own address, which is what lets an
/// update succeed on a machine whose only route out is the core itself.
async fn update_profiles(service: Arc<Mutex<Service>>, uids: Vec<String>, sink: &EventSink) {
    let (proxy, store, targets) = {
        let guard = service
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let proxy = guard.proxy_addr();
        let Ok(store) = guard.store() else {
            let _ = sink.send(Event::Failed("could not read the profile index".to_owned()));
            return;
        };
        let targets: Vec<String> = if uids.is_empty() {
            due_remote_uids(store.items(), chrono::Utc::now().timestamp())
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
            let _ = sink.send(Event::Failed(error.to_string()));
            return;
        }
    };

    let (mut updated, mut failed) = (0usize, 0usize);
    for uid in targets {
        let result = match fetcher.prepare(&store, &uid).await {
            Ok(prepared) => {
                let guard = service
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                guard
                    .store()
                    .and_then(|mut current| prepared.commit(&mut current))
            }
            Err(error) => Err(error),
        };
        match result {
            Ok(_) => updated += 1,
            Err(error) => {
                tracing::debug!(profile = %uid, error = %error, "update failed");
                failed += 1;
            }
        }
    }
    let _ = sink.send(Event::Done(Done::ProfilesUpdated { updated, failed }));
}

fn due_remote_uids(items: &[PrfItem], now: i64) -> Vec<String> {
    items
        .iter()
        .filter(|item| item.kind == ProfileType::Remote && is_due(item, now))
        .map(|item| item.uid.clone())
        .collect()
}

type NodeEndpoints = HashMap<String, (String, u16)>;

fn node_endpoints(service: &Service) -> Result<NodeEndpoints, Error> {
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
                self.client
                    .proxy_delay(name, &self.settings.url, self.settings.timeout_ms, expected)
                    .await
                    .map_err(|error| error.to_string())
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

async fn tcp_connect_ms(host: &str, port: u16, timeout_ms: u32) -> Result<u16, String> {
    let began = Instant::now();
    tokio::time::timeout(
        Duration::from_millis(u64::from(timeout_ms)),
        tokio::net::TcpStream::connect((host, port)),
    )
    .await
    .map_err(|_| "TCP connection timed out".to_owned())?
    .map_err(|error| error.to_string())?;
    Ok(u16::try_from(began.elapsed().as_millis())
        .unwrap_or(u16::MAX)
        .max(1))
}

async fn icmp_echo(host: &str, timeout_ms: u32) -> Result<u16, String> {
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

fn parse_ping_ms(output: &str) -> Option<u16> {
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
        TestKind::CoreHealth => (
            match probe.client.version().await {
                Ok(version) => TestResult::Passed(version.trimmed().to_owned()),
                Err(error) => TestResult::Failed(error.to_string()),
            },
            1,
        ),
        TestKind::DnsLookup => (
            match probe.client.dns_query(target, "A").await {
                Ok(answer) => {
                    TestResult::Passed(format!("answer of {} bytes", answer.to_string().len()))
                }
                Err(error) => TestResult::Failed(error.to_string()),
            },
            1,
        ),
        TestKind::Bandwidth => (
            match download_speed(probe.proxy_addr).await {
                Ok(mbps) => TestResult::Passed(format!("{mbps:.1} Mbit/s")),
                Err(error) => TestResult::Failed(error),
            },
            1,
        ),
    }
}

/// A bounded download through the deployed HTTP/mixed listener. The current
/// routing mode and selected policy determine the exit; no group is mutated.
async fn download_speed(proxy_addr: Option<&str>) -> Result<f64, String> {
    let address =
        proxy_addr.ok_or_else(|| "no HTTP or mixed proxy listener is deployed".to_owned())?;
    let proxy =
        reqwest::Proxy::all(format!("http://{address}")).map_err(|error| error.to_string())?;
    let client = reqwest::Client::builder()
        .proxy(proxy)
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|error| error.to_string())?;
    let began = Instant::now();
    let response = client
        .get("https://speed.cloudflare.com/__down?bytes=4000000")
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?;
    let mut stream = response.bytes_stream();
    let mut bytes = 0_usize;
    while let Some(chunk) = stream.next().await {
        bytes += chunk.map_err(|error| error.to_string())?.len();
        if bytes >= 4_000_000 {
            break;
        }
    }
    if bytes < 1_000_000 {
        return Err(format!("speed test returned only {bytes} bytes"));
    }
    Ok((bytes as f64 * 8.0) / began.elapsed().as_secs_f64() / 1_000_000.0)
}

/// Start or restart, keeping the service lock only for the local operation.
fn launch_core(service: &Service, restart: bool) -> Result<(u32, String, Option<Client>), Error> {
    let pid = if restart {
        service.restart_core()?
    } else {
        service.start_core()?
    };
    let text = service.paths().read(&service.paths().runtime_config())?;
    let configured_mode = cvt_core::model::config::Config::from_yaml(&text)?.mode();
    let client = service.endpoint()?.map(Client::new).transpose()?;
    Ok((pid, configured_mode, client))
}

/// Probe the listener after releasing the service lock.
async fn ready_mode(launch: (u32, String, Option<Client>)) -> Result<(u32, String), Error> {
    let (pid, configured_mode, client) = launch;
    let Some(client) = client else {
        return Ok((pid, configured_mode));
    };
    client.wait_until_ready().await?;
    let mode = client
        .configs()
        .await
        .ok()
        .and_then(|config| config.mode)
        .unwrap_or(configured_mode);
    Ok((pid, mode))
}

/// Flatten selectable groups and their members into rows.
///
/// The core also reports internal adapters and an always-present GLOBAL group.
/// They have no useful action in rule/direct mode and standalone adapters
/// cannot be selected, so neither belongs in the interactive list.
fn node_rows(proxies: &BTreeMap<String, ProxyView>, mode: Option<&str>) -> Vec<NodeRow> {
    let mut rows = Vec::new();
    for view in proxies
        .values()
        .filter(|view| view.is_group() && (view.name != "GLOBAL" || mode == Some("global")))
    {
        rows.push(NodeRow::from_group(view));
        for name in view.members() {
            if let Some(member) = proxies.get(name) {
                rows.push(NodeRow::from_member(
                    member,
                    &view.name,
                    view.now.as_deref() == Some(name.as_str())
                        || view.fixed.as_deref() == Some(name.as_str()),
                ));
            }
        }
    }
    rows
}

/// Show the selected document while Mihomo is stopped. Live health and
/// provider-expanded membership become available once the core starts.
fn config_node_rows(config: &cvt_core::model::config::Config) -> Vec<NodeRow> {
    use cvt_core::model::proxy::GroupKind;

    let proxies = config.proxies();
    let groups = config.proxy_groups();
    let mut rows = Vec::new();
    let mut referenced = HashSet::new();
    for group in &groups {
        let kind = match GroupKind::from_wire(&group.kind) {
            GroupKind::Select => "Selector",
            GroupKind::UrlTest => "URLTest",
            GroupKind::Fallback => "Fallback",
            GroupKind::LoadBalance => "LoadBalance",
            GroupKind::Unknown => group.kind.as_str(),
        };
        rows.push(NodeRow {
            name: group.name.clone(),
            kind: kind.to_owned(),
            group: None,
            delay: None,
            alive: false,
            active: false,
            is_group: true,
            is_proxy: false,
            members: group.proxies.len(),
            selectable: false,
        });
        for name in &group.proxies {
            referenced.insert(name.as_str());
            let kind = proxies
                .iter()
                .find(|proxy| proxy.name == *name)
                .map_or_else(
                    || {
                        groups
                            .iter()
                            .find(|candidate| candidate.name == *name)
                            .map_or("builtin", |candidate| candidate.kind.as_str())
                    },
                    |proxy| proxy.kind.as_str(),
                );
            rows.push(NodeRow {
                name: name.clone(),
                kind: kind.to_owned(),
                group: Some(group.name.clone()),
                delay: None,
                alive: false,
                active: false,
                is_group: false,
                is_proxy: proxies.iter().any(|proxy| {
                    proxy.name == *name && proxy.server.is_some() && proxy.port.is_some()
                }),
                members: 0,
                selectable: false,
            });
        }
    }
    for proxy in &proxies {
        if !referenced.contains(proxy.name.as_str()) {
            rows.push(NodeRow::from_config_proxy(proxy));
        }
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn parses_icmp_round_trip_without_mistaking_packet_statistics_for_latency() {
        assert_eq!(parse_ping_ms("64 bytes: time=12.4 ms\n1 packets"), Some(13));
        assert_eq!(parse_ping_ms("64 bytes: time=0,8 ms"), Some(1));
        assert_eq!(parse_ping_ms("Reply from 127.0.0.1: time<1ms"), Some(1));
        assert_eq!(parse_ping_ms("1 packets transmitted, 0 received"), None);
    }

    #[tokio::test]
    async fn tcp_probe_measures_an_open_listener_and_rejects_a_closed_one() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(tcp_connect_ms("127.0.0.1", port, 500).await.is_ok());
        drop(listener);
        assert!(tcp_connect_ms("127.0.0.1", 0, 500).await.is_err());
    }

    #[test]
    fn live_inventory_places_each_member_under_its_group() {
        let inventory: cvt_core::mihomo::types::ProxiesResponse = serde_json::from_value(
            serde_json::json!({"proxies": {
                "GROUP": {"name":"GROUP", "type":"Selector", "all":["node-a", "DIRECT"], "now":"node-a"},
                "node-a": {"name":"node-a", "type":"Vless", "alive":true},
                "DIRECT": {"name":"DIRECT", "type":"Direct", "alive":true}
            }}),
        )
        .unwrap();
        let rows = node_rows(&inventory.proxies, Some("rule"));
        assert_eq!(rows.len(), 3);
        assert!(rows[0].is_group);
        assert_eq!(rows[0].members, 2);
        assert_eq!(rows[1].group.as_deref(), Some("GROUP"));
        assert!(rows[1].active);
        assert_eq!(rows[2].group.as_deref(), Some("GROUP"));
    }

    #[test]
    fn internal_adapters_are_hidden_and_global_only_appears_in_global_mode() {
        let inventory: cvt_core::mihomo::types::ProxiesResponse =
            serde_json::from_value(serde_json::json!({"proxies": {
                "GLOBAL": {"name":"GLOBAL", "type":"Selector", "all":["node-a"]},
                "GROUP": {"name":"GROUP", "type":"Selector", "all":["node-a"]},
                "node-a": {"name":"node-a", "type":"Vless"},
                "COMPATIBLE": {"name":"COMPATIBLE", "type":"Compatible"},
                "PASS": {"name":"PASS", "type":"Pass"},
                "PASS-RULE": {"name":"PASS-RULE", "type":"PassRule"},
                "REJECT-DROP": {"name":"REJECT-DROP", "type":"RejectDrop"}
            }}))
            .unwrap();
        let rule = node_rows(&inventory.proxies, Some("rule"));
        assert_eq!(rule.iter().filter(|row| row.is_group).count(), 1);
        assert!(
            rule.iter()
                .all(|row| row.name != "GLOBAL" && row.name != "PASS")
        );
        let global = node_rows(&inventory.proxies, Some("global"));
        assert_eq!(global.iter().filter(|row| row.is_group).count(), 2);
        assert!(global.iter().any(|row| row.name == "GLOBAL"));
        assert!(global.iter().all(|row| row.name != "COMPATIBLE"));
    }

    #[test]
    fn selected_document_has_group_members_even_without_a_core() {
        let config = cvt_core::model::config::Config::from_yaml(
            "proxies:\n  - {name: node-a, type: vless}\nproxy-groups:\n  - {name: GROUP, type: select, proxies: [node-a, DIRECT]}\n",
        )
        .unwrap();
        let rows = config_node_rows(&config);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].members, 2);
        assert_eq!(rows[1].group.as_deref(), Some("GROUP"));
        assert_eq!(rows[2].name, "DIRECT");
    }

    #[test]
    fn all_due_selects_only_remote_profiles_whose_interval_elapsed() {
        let now = 1_000_000;
        let mut due = PrfItem::remote("due", "due", "https://example.com/a");
        due.option.allow_auto_update = Some(true);
        due.option.update_interval = Some(60);
        due.updated = Some(now - 3_600);

        let mut fresh = PrfItem::remote("fresh", "fresh", "https://example.com/b");
        fresh.option.allow_auto_update = Some(true);
        fresh.option.update_interval = Some(60);
        fresh.updated = Some(now - 1);

        let mut disabled = PrfItem::remote("disabled", "disabled", "https://example.com/c");
        disabled.option.allow_auto_update = Some(false);
        disabled.option.update_interval = Some(60);

        let mut local = PrfItem::local("local", "local");
        local.option.allow_auto_update = Some(true);
        local.option.update_interval = Some(60);

        assert_eq!(
            due_remote_uids(&[fresh, disabled, local, due], now),
            vec!["due"]
        );
    }

    /// Every effect the interface can ask for is executed, and every arm names
    /// an effect that exists.
    ///
    /// Textual, and for the same reason the diagnostic scan is: the enum and
    /// the match are two lists in two crates that have to agree, and nothing in
    /// the type system makes them. An effect with no arm is a key that appears
    /// to work and does nothing — which is what a `match` with a `_ => {}` arm
    /// looks like from the outside.
    #[test]
    fn every_effect_is_executed_and_every_arm_names_an_effect() {
        let app = include_str!("../../cvt-tui/src/app.rs");
        let defined: Vec<&str> = app
            .split("pub enum Effect {")
            .nth(1)
            .unwrap_or_default()
            .split("\n}")
            .next()
            .unwrap_or_default()
            .lines()
            .filter_map(|line| {
                let name = line
                    .trim()
                    .split(['(', '{', ','])
                    .next()
                    .unwrap_or_default()
                    .trim();
                (line.starts_with("    ")
                    && !line.starts_with("     ")
                    && name.chars().next().is_some_and(char::is_uppercase))
                .then_some(name)
            })
            .collect();
        assert!(
            defined.len() > 20,
            "the scan found only {} effects, so it is looking in the wrong place",
            defined.len()
        );

        let source = include_str!("executor.rs");
        let handled: Vec<&str> = source
            .match_indices("Effect::")
            .filter_map(|(at, _)| {
                let rest = &source[at + "Effect::".len()..];
                let name: String = rest
                    .chars()
                    .take_while(char::is_ascii_alphanumeric)
                    .collect();
                (!name.is_empty()).then_some(Box::leak(name.into_boxed_str()) as &str)
            })
            .collect();

        let unexecuted: Vec<&&str> = defined
            .iter()
            .filter(|name| !handled.contains(*name))
            .collect();
        assert!(
            unexecuted.is_empty(),
            "these effects are defined and no arm executes them: {unexecuted:?}"
        );

        let unknown: Vec<&&str> = handled
            .iter()
            .filter(|name| !defined.contains(*name))
            .collect();
        assert!(
            unknown.is_empty(),
            "the executor names effects that do not exist: {unknown:?}"
        );
    }
}
