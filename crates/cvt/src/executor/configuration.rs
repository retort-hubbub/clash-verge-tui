//! Configuration deployment effects. Both operations share generation and commit.

use super::Executor;
use cvt_core::{ReloadMode, Result, Service};
use cvt_tui::{Done, Event, EventSink};
use std::sync::Arc;

impl Executor {
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
