//! Profiles adapter for TUI effects.

use cvt_core::Service;
use cvt_core::profile::item::{PrfItem, ProfileType};
use cvt_core::profile::source::{SubscriptionFetcher, is_due};
use cvt_tui::{Done, Event, EventSink};
use std::sync::{Arc, Mutex};

/// Fresh copies of the given profiles, or of every due remote one when the
/// list is empty.
///
/// The fetcher is built with the core's own address, which is what lets an
/// update succeed on a machine whose only route out is the core itself.
pub(super) async fn update_profiles(
    service: Arc<Mutex<Service>>,
    uids: Vec<String>,
    sink: &EventSink,
) {
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

pub(super) fn due_remote_uids(items: &[PrfItem], now: i64) -> Vec<String> {
    items
        .iter()
        .filter(|item| item.kind == ProfileType::Remote && is_due(item, now))
        .map(|item| item.uid.clone())
        .collect()
}
