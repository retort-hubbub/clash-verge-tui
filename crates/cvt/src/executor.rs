//! Execute typed TUI effects using the shared Service facade.
//!
//! Domain handlers live in private modules; this module owns dispatch and task
//! coordination. Network work reports through EventSink. Blocking workflows
//! run on workers and may hold the service mutex to serialize configuration
//! changes; short local operations are handled synchronously.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use cvt_core::mihomo::client::Client;
use cvt_core::mihomo::types::ConfigPatch;
use cvt_core::profile::item::{PrfItem, ProfileType};
use cvt_core::{AppPaths, Error, Service};
use cvt_tui::row::{ProfileRow, TestKind};
use cvt_tui::{Data, Done, Effect, Event, EventSink, Screen};

mod adapters;
mod application_update;
mod configuration;
mod diagnostics;
mod inventory;
mod lifecycle;
mod permissions;
mod probes;
mod profile_editing;
mod profiles;
mod refresh;
mod selection;
mod settings;
mod streams;
#[cfg(test)]
mod tests;

use adapters::{open_editor, preview_of};
#[cfg(test)]
use diagnostics::{parse_exit_ip, speedtest_download_mbps};
#[cfg(test)]
use inventory::{config_node_rows, node_rows};
#[cfg(test)]
use probes::{is_benchmark_address, parse_ping_ms, tcp_connect_ms};
#[cfg(test)]
use profiles::due_remote_uids;
use profiles::update_profiles;

/// Performs effects against a service.
#[derive(Clone)]
pub struct Executor {
    service: Arc<Mutex<Service>>,
    app_update_busy: Arc<AtomicBool>,
    app_installed: Arc<Mutex<Option<String>>>,
    /// Suppress controller reads between process launch and API readiness.
    starting: Arc<AtomicBool>,
    /// Bumped when the user cancels; a batch notices at the next node instead
    /// of running to the end of a list they no longer want.
    test_epoch: Arc<AtomicU64>,
    /// Whether the live stream has been started, so it starts once rather than
    /// once per refresh.
    streaming: Arc<AtomicBool>,
    /// Last public-IP request, to avoid polling an external service each UI tick.
    ip_last_fetch: Arc<Mutex<Option<Instant>>>,
    /// Discard IP lookups started before a routing change.
    ip_epoch: Arc<AtomicU64>,
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
            app_update_busy: Arc::new(AtomicBool::new(false)),
            app_installed: Arc::new(Mutex::new(None)),
            starting: Arc::new(AtomicBool::new(false)),
            test_epoch: Arc::new(AtomicU64::new(0)),
            streaming: Arc::new(AtomicBool::new(false)),
            ip_last_fetch: Arc::new(Mutex::new(None)),
            ip_epoch: Arc::new(AtomicU64::new(0)),
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
                if matches!(effect, Effect::AuthorizeCore { .. }) {
                    let failure_sink = sink.clone();
                    if let Err(error) =
                        tokio::task::spawn_blocking(move || this.perform(effect, &sink)).await
                    {
                        Self::emit(&failure_sink, Event::Failed(error.to_string()));
                    }
                } else {
                    this.perform(effect, &sink);
                }
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
        self.with_service(|service| {
            if !service.core_status().is_running() {
                return Err(Error::ControllerUnreachable {
                    endpoint: service
                        .endpoint()?
                        .map_or_else(|| "unknown".to_owned(), |endpoint| endpoint.describe()),
                    source: "no running core owned by this application home".into(),
                });
            }
            service.client()
        })
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

    /// Invalidate both cached data and responses from earlier IP requests.
    fn invalidate_ip(&self) {
        self.ip_epoch.fetch_add(1, Ordering::SeqCst);
        *self
            .ip_last_fetch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }

    /// Do one effect.
    fn perform(&self, effect: Effect, sink: &EventSink) {
        if matches!(
            effect,
            Effect::StartCore
                | Effect::RestartCore
                | Effect::ApplyConfig { .. }
                | Effect::SaveSettings { .. }
        ) && self.request_permissions(&effect, sink)
        {
            return;
        }
        match effect {
            Effect::AuthorizeCore {
                binary,
                capabilities,
                next,
            } => {
                let event = match crate::tun::authorize_capabilities(&binary, &capabilities) {
                    Ok(()) => Event::Data(Data::CoreAuthorized { next }),
                    Err(error) => Event::Failed(error.to_string()),
                };
                Self::emit(sink, event);
            }
            Effect::EditProfileSource { uid, url } => self.edit_profile_source(uid, url, sink),
            Effect::EditProfileOverride { uid } => self.edit_profile_override(uid.as_deref(), sink),
            Effect::AddProfileRule { rule } => self.add_profile_rule(&rule, sink),
            // The loop stops on `App::is_quit`; there is nothing to perform.
            Effect::Quit => {}

            Effect::Startup => self.startup(sink),
            Effect::CheckAppUpdate { manual } => self.check_app_update(manual, sink),
            Effect::InstallAppUpdate { tag } => self.install_app_update(tag, sink),
            Effect::DismissAppUpdate { tag, skip } => self.dismiss_app_update(&tag, skip, sink),

            // ---- refresh, which fans out per screen
            Effect::Refresh(screen) => self.refresh(screen, sink),
            Effect::RefreshIp => {
                self.invalidate_ip();
                self.refresh(Screen::Home, sink);
            }
            Effect::StartCore | Effect::RestartCore => {
                self.start_core(matches!(effect, Effect::RestartCore), sink);
            }

            // ---- everything that talks to the core is spawned
            // The two selection effects are hand-written rather than going
            // through `spawn_net`, because they touch the profile index as well
            // as the core: the choice is recorded so that the next apply, which
            // rebuilds every group, does not throw it away.
            Effect::SelectNode { group, member } => self.select_node(group, member, sink),
            Effect::ClearNodePin { group } => self.clear_node_pin(group, sink),
            Effect::CloseConnection { id } => {
                let completed_id = id.clone();
                self.spawn_net(
                    sink,
                    move |client| async move { client.close_connection(&id).await },
                    move |()| Event::Done(Done::ConnectionClosed { id: completed_id }),
                );
            }
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
            Effect::SetCoreMode { mode } => {
                self.invalidate_ip();
                self.spawn_net(
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
                );
            }
            Effect::UpgradeCore => self.upgrade_core(sink),
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
            Effect::TestRouteSpeed { mode } => self.spawn_route_speed(mode, sink),
            Effect::InstallSpeedtestGo => {
                let home = self.with_service(|service| service.paths().home().to_path_buf());
                let sink = sink.clone();
                tokio::spawn(async move {
                    match crate::speedtest::install(&home).await {
                        Ok(version) => {
                            let _ = sink.send(Event::Done(Done::SpeedtestInstalled { version }));
                        }
                        Err(error) => {
                            let _ = sink.send(Event::Failed(error));
                        }
                    }
                });
            }
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
            Effect::EditProfile { uid } => self.edit_profile_document(&uid, sink),

            Effect::ExportLogs { path, contents } => match std::fs::write(&path, contents) {
                Ok(()) => Self::emit(
                    sink,
                    Event::Data(Data::Notice(format!("log written to {}", path.display()))),
                ),
                Err(error) => Self::emit(sink, Event::Failed(Error::io(&path, error).to_string())),
            },

            // ---- applying runs on a blocking thread, and reports once
            Effect::ApplyConfig { mode } => self.apply_config(mode, sink),
            Effect::PrepareConfig => self.prepare_config(sink),
            Effect::SynchronizeConfig => self.synchronize_config(sink),

            // Enumerate local commands so adding an effect requires a handler.
            other @ (Effect::LoadProfiles
            | Effect::SwitchProfile { .. }
            | Effect::SetChain { .. }
            | Effect::DeleteProfile { .. }
            | Effect::RenameProfile { .. }
            | Effect::NewProfile { .. }
            | Effect::DetectImportSources
            | Effect::ImportProfiles { .. }
            | Effect::PreviewConfig
            | Effect::RollbackConfig
            | Effect::StopCore
            | Effect::SaveSettings { .. }) => {
                if matches!(other, Effect::StopCore) {
                    self.ip_epoch.fetch_add(1, Ordering::SeqCst);
                }
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
    fn local(&self, effect: Effect, sink: &EventSink) -> Result<Event, Error> {
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
            Effect::SaveSettings { settings } => Ok(self.save_settings(settings, sink)),
            other => Err(Error::Unsupported(format!(
                "the interactive interface does not perform {other:?} yet"
            ))),
        }
    }
}
