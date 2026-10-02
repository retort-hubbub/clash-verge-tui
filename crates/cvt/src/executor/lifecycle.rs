//! Lifecycle adapter for TUI effects.

use super::Executor;
use super::profiles::update_profiles;
use cvt_core::mihomo::client::Client;
use cvt_core::{Error, Service};
use cvt_tui::{Data, Done, Event, EventSink};
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// Start or restart on a blocking worker; release the service lock before
/// waiting for the controller API.
pub(super) fn launch_core(
    service: &Service,
    restart: bool,
) -> Result<(u32, String, Option<Client>), Error> {
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
pub(super) async fn ready_mode(
    launch: (u32, String, Option<Client>),
) -> Result<(u32, String), Error> {
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

impl Executor {
    pub(super) fn startup(&self, sink: &EventSink) {
        self.check_app_update(false, sink);
        let (auto_start, update_on_start) = self.with_service(|service| {
            (
                service.settings().core.auto_start
                    && service.store().is_ok_and(|store| store.current().is_some())
                    && !service.core_status().is_running(),
                service.settings().update.update_on_start,
            )
        });
        if auto_start {
            self.perform(cvt_tui::Effect::StartCore, sink);
        }
        if update_on_start {
            let service = Arc::clone(&self.service);
            let sink = sink.clone();
            tokio::spawn(async move {
                update_profiles(service, Vec::new(), &sink).await;
            });
        }
    }
}

impl Executor {
    pub(super) fn start_core(&self, restart: bool, sink: &EventSink) {
        self.invalidate_ip();
        let service = Arc::clone(&self.service);
        let health_service = Arc::clone(&self.service);
        let starting = Arc::clone(&self.starting);
        let sink = sink.clone();
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
                Ok(Ok(launch)) => {
                    let pid = launch.0;
                    let ready = ready_mode(launch).await;
                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                    let guard = health_service
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let health = guard.supervisor().check_health();
                    let result = ready.and_then(|value| health.map(|()| value));
                    if result.is_err()
                        && matches!(guard.core_status(), cvt_core::mihomo::CoreStatus::Running { pid: current, .. } if current == pid)
                    {
                        let _ = guard.stop_core();
                    }
                    result
                }
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
}

impl Executor {
    pub(super) fn upgrade_core(&self, sink: &EventSink) {
        let service = Arc::clone(&self.service);
        let sink = sink.clone();
        tokio::spawn(async move {
            let (paths, was_running, proxy) = {
                let guard = service
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                (
                    guard.paths().clone(),
                    guard.core_status().is_running(),
                    guard.proxy_addr(),
                )
            };
            let _ = sink.send(Event::Data(Data::Notice(
                "downloading latest mihomo core...".to_owned(),
            )));
            let outcome = cvt_core::mihomo::download::install_latest_core_with_proxy(
                &paths,
                proxy.as_deref(),
            )
            .await;
            match outcome {
                Ok(version) => {
                    let status = {
                        let guard = service
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        guard.core_status()
                    };
                    let _ = sink.send(Event::Data(Data::Core(status)));
                    let _ = sink.send(Event::Done(Done::CoreUpgraded { version }));
                    if was_running {
                        let _ = sink.send(Event::Data(Data::CoreAuthorized {
                            next: Box::new(cvt_tui::Effect::RestartCore),
                        }));
                    }
                }
                Err(error) => {
                    let _ = sink.send(Event::Failed(error.to_string()));
                }
            }
        });
    }
}
