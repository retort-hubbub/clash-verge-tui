//! Configuration deployment effects. Both operations share generation and commit.

use super::Executor;
use cvt_core::{ReloadMode, Result, Service};
use cvt_tui::{Done, Effect, Event, EventSink};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

impl Executor {
    pub(super) fn synchronize_config(&self, sink: &EventSink) {
        let executor = self.clone();
        let sink = sink.clone();
        tokio::spawn(async move {
            // A download may finish between process launch and API readiness.
            // Decide using the service state, not the last TUI status event.
            let ready = tokio::time::timeout(Duration::from_secs(15), async {
                while executor.starting.load(Ordering::SeqCst) {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            })
            .await;
            if ready.is_err() {
                Self::emit(
                    &sink,
                    Event::Failed(
                        "core startup timed out before configuration synchronization".to_owned(),
                    ),
                );
                return;
            }
            let effect = executor.with_service(|service| {
                if service.core_status().is_running() {
                    Effect::ApplyConfig {
                        mode: if service.settings().update.prefer_hot_reload {
                            ReloadMode::Auto
                        } else {
                            ReloadMode::Restart
                        },
                    }
                } else {
                    Effect::PrepareConfig
                }
            });
            executor.perform(effect, &sink);
        });
    }

    pub(super) fn apply_config(&self, mode: ReloadMode, sink: &EventSink) {
        self.invalidate_ip();
        self.deploy_config(Some(mode), sink);
    }

    pub(super) fn prepare_config(&self, sink: &EventSink) {
        self.deploy_config(None, sink);
    }

    fn deploy_config(&self, mode: Option<ReloadMode>, sink: &EventSink) {
        let service = Arc::clone(&self.service);
        let sink = sink.clone();
        tokio::task::spawn_blocking(move || {
            let outcome = {
                let guard = service
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                deploy(&guard, mode)
            };
            let event = match outcome {
                Ok(done) => Event::Done(done),
                Err(error) => Event::Failed(error.to_string()),
            };
            let _ = sink.send(event);
        });
    }
}

/// Serialized worker operation; readiness waits must stay off the event loop.
fn deploy(service: &Service, mode: Option<ReloadMode>) -> Result<Done> {
    let outcome = service.generate()?;
    let changed = outcome.diff.entries.len();
    service.pipeline().commit(&outcome, false)?;
    let reload = mode
        .map(|mode| tokio::runtime::Handle::current().block_on(service.reload(mode)))
        .transpose()?;
    Ok(Done::ConfigApplied { reload, changed })
}
