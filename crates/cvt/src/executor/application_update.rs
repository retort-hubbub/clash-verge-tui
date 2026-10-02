//! Application updates are separate from managed Mihomo updates.
use super::Executor;
use cvt_tui::{Data, Done, Event, EventSink};
use std::sync::atomic::Ordering;

impl Executor {
    pub(super) fn check_app_update(&self, manual: bool, sink: &EventSink) {
        if self.app_update_busy.swap(true, Ordering::SeqCst) {
            if manual {
                Self::emit(
                    sink,
                    Event::Data(Data::Notice(
                        "an application update is already in progress".to_owned(),
                    )),
                );
            }
            return;
        }
        let (paths, proxy) = self.with_service(|service| {
            (
                service.paths().clone(),
                if service.core_status().is_running() {
                    service.proxy_addr()
                } else {
                    None
                },
            )
        });
        let installed = self
            .app_installed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let sink = sink.clone();
        let busy = std::sync::Arc::clone(&self.app_update_busy);
        tokio::spawn(async move {
            let result = crate::app_update::check(&paths, proxy, manual, installed).await;
            busy.store(false, Ordering::SeqCst);
            match result {
                Ok(Some(release)) => {
                    Self::emit(&sink, Event::Data(Data::AppUpdateAvailable(release)));
                }
                Ok(None) if manual => Self::emit(
                    &sink,
                    Event::Data(Data::Notice(
                        "no newer application update is available".to_owned(),
                    )),
                ),
                Err(error) if manual => Self::emit(
                    &sink,
                    Event::Failed(format!("application update: {error:#}")),
                ),
                Err(error) => tracing::debug!(%error, "application update check failed"),
                Ok(None) => {}
            }
        });
    }

    pub(super) fn dismiss_app_update(&self, tag: &str, skip: bool, sink: &EventSink) {
        if let Err(error) =
            self.with_service(|service| crate::app_update::dismiss(service.paths(), tag, skip))
        {
            Self::emit(
                sink,
                Event::Failed(format!("application update: {error:#}")),
            );
        }
    }

    pub(super) fn install_app_update(&self, tag: String, sink: &EventSink) {
        if self.app_update_busy.swap(true, Ordering::SeqCst) {
            return;
        }
        let proxy = self.with_service(|service| {
            if service.core_status().is_running() {
                service.proxy_addr()
            } else {
                None
            }
        });
        let installed = std::sync::Arc::clone(&self.app_installed);
        let busy = std::sync::Arc::clone(&self.app_update_busy);
        let sink = sink.clone();
        tokio::spawn(async move {
            match crate::app_update::install(&tag, proxy).await {
                Ok(backup) => {
                    *installed
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(tag.clone());
                    Self::emit(
                        &sink,
                        Event::Done(Done::AppUpdated {
                            version: tag,
                            backup,
                        }),
                    );
                }
                Err(error) => Self::emit(
                    &sink,
                    Event::Failed(format!("application update: {error:#}")),
                ),
            }
            busy.store(false, Ordering::SeqCst);
        });
    }
}
