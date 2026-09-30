//! Persist settings, then apply the resulting runtime change.

use std::sync::Arc;

use cvt_core::{AppPaths, Error, Service, Settings};
use cvt_tui::{Data, Done, Event, EventSink};

use super::Executor;

/// The runtime work remaining after preferences have been saved.
struct SettingsChange {
    paths: AppPaths,
    tun_changed: bool,
    tun_enabled: bool,
}

impl Executor {
    pub(super) fn save_settings(&self, settings: Settings, sink: &EventSink) -> Event {
        let service = Arc::clone(&self.service);
        let sink = sink.clone();
        tokio::spawn(async move {
            let prepared = tokio::task::spawn_blocking(move || {
                let mut service = service
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                persist(&mut service, settings)
            })
            .await;
            let change = match prepared {
                Ok(Ok(change)) => change,
                Ok(Err(error)) => {
                    Self::emit(&sink, Event::Failed(error.to_string()));
                    return;
                }
                Err(error) => {
                    Self::emit(&sink, Event::Failed(error.to_string()));
                    return;
                }
            };
            Self::emit(&sink, Event::Done(Done::SettingsSaved));
            if change.tun_changed {
                let event = match change.apply_tun().await {
                    Ok(()) => Event::Data(Data::Notice("TUN configuration applied".to_owned())),
                    Err(error) => {
                        Event::Failed(format!("settings saved; TUN could not be applied: {error}"))
                    }
                };
                Self::emit(&sink, event);
            }
        });
        Event::Data(Data::Notice("saving settings…".to_owned()))
    }
}

fn persist(service: &mut Service, settings: Settings) -> Result<SettingsChange, Error> {
    let previous = &service.settings().core;
    let requested = &settings.core;
    if previous.login_autostart != requested.login_autostart {
        let exe = std::env::current_exe().map_err(|error| Error::Unsupported(error.to_string()))?;
        crate::autostart::apply(requested.login_autostart, service.paths().home(), &exe)
            .map_err(|error| Error::Unsupported(error.to_string()))?;
    }
    settings.save(service.paths())?;
    let change = SettingsChange {
        paths: service.paths().clone(),
        tun_changed: previous.tun_enabled != requested.tun_enabled,
        tun_enabled: requested.tun_enabled == Some(true),
    };
    service.set_settings(settings);
    Ok(change)
}

impl SettingsChange {
    async fn apply_tun(self) -> Result<(), Error> {
        let service = Service::open(self.paths)?;
        if service.store()?.current().is_none() {
            return Ok(());
        }
        if service.core_status().is_running() {
            let mode = if self.tun_enabled {
                cvt_core::ReloadMode::Restart
            } else {
                cvt_core::ReloadMode::Auto
            };
            service.apply(false, mode).await?;
        } else {
            let generated = service.generate()?;
            service.pipeline().commit(&generated, false)?;
        }
        Ok(())
    }
}
