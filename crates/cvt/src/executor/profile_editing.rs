//! Source URL edits and overrides owned by a base subscription.

use super::{Executor, adapters::open_editor};
use cvt_core::profile::source::SubscriptionFetcher;
use cvt_core::{Error, Result};
use cvt_tui::{Done, Event, EventSink};
use std::sync::Arc;

impl Executor {
    pub(super) fn edit_profile_document(&self, uid: &str, sink: &EventSink) {
        let result = self.profile_path(uid).and_then(|path| open_editor(&path));
        if let Err(error) = result {
            Self::emit(sink, Event::Failed(error.to_string()));
            return;
        }
        let active = self.with_service(|service| {
            service.store().is_ok_and(|store| {
                store.current_uid() == Some(uid)
                    || store
                        .resolve_chain()
                        .is_ok_and(|chain| chain.iter().any(|item| item.uid == uid))
            })
        });
        if active {
            Self::emit(sink, Event::Done(Done::ProfileContentChanged));
        } else {
            Self::emit(
                sink,
                Event::Data(cvt_tui::Data::Notice(format!("edited {uid}"))),
            );
        }
    }

    pub(super) fn edit_profile_source(&self, uid: String, url: String, sink: &EventSink) {
        let snapshot = self.with_service(|service| -> Result<_> {
            let mut store = service.store()?;
            let previous = store.get(&uid).and_then(|item| item.url.clone());
            store.set_url(&uid, &url)?;
            Ok((store, previous, service.proxy_addr()))
        });
        let (store, previous, proxy) = match snapshot {
            Ok(snapshot) => snapshot,
            Err(error) => {
                Self::emit(sink, Event::Failed(error.to_string()));
                return;
            }
        };
        let service = Arc::clone(&self.service);
        let sink = sink.clone();
        tokio::spawn(async move {
            let prepared = match SubscriptionFetcher::new(proxy) {
                Ok(fetcher) => fetcher.prepare(&store, &uid).await,
                Err(error) => Err(error),
            };
            let result = prepared.and_then(|prepared| {
                let mut current = service
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .store()?;
                if current.get(&uid).and_then(|item| item.url.as_ref()) != previous.as_ref() {
                    return Err(Error::invalid(
                        "subscription URL",
                        "changed while downloading; retry the edit",
                    ));
                }
                current.set_url(&uid, &url)?;
                prepared.commit(&mut current)?;
                let active = current.current_uid() == Some(uid.as_str())
                    || current
                        .resolve_chain()
                        .is_ok_and(|chain| chain.iter().any(|item| item.uid == uid));
                Ok(active)
            });
            match result {
                Ok(active) => {
                    Self::emit(
                        &sink,
                        Event::Done(Done::ProfilesUpdated {
                            updated: 1,
                            failed: 0,
                        }),
                    );
                    if active {
                        Self::emit(&sink, Event::Done(Done::ProfileContentChanged));
                    }
                }
                Err(error) => Self::emit(&sink, Event::Failed(error.to_string())),
            }
        });
    }

    pub(super) fn edit_profile_override(&self, uid: Option<&str>, sink: &EventSink) {
        let prepared = self.with_service(|service| -> Result<_> {
            let mut store = service.store()?;
            let uid = uid
                .or_else(|| store.current_uid())
                .ok_or_else(|| Error::invalid("override", "select a base profile first"))?
                .to_owned();
            let item = store.ensure_override(&uid)?;
            Ok((
                cvt_core::profile::store::document_path(service.paths(), &item),
                store.current_uid() == Some(uid.as_str()),
            ))
        });
        let result = prepared.and_then(|(path, active)| {
            open_editor(&path)?;
            Ok(active)
        });
        match result {
            Ok(true) => Self::emit(sink, Event::Done(Done::ProfileContentChanged)),
            Ok(false) => {
                if let Ok(rows) = self.with_service(|service| {
                    service
                        .store()
                        .map(|store| cvt_tui::row::ProfileRow::all(&store))
                }) {
                    Self::emit(sink, Event::Data(cvt_tui::Data::Profiles(rows)));
                }
                Self::emit(
                    sink,
                    Event::Data(cvt_tui::Data::Notice("profile override saved".to_owned())),
                );
            }
            Err(error) => Self::emit(sink, Event::Failed(error.to_string())),
        }
    }

    pub(super) fn add_profile_rule(&self, rule: &str, sink: &EventSink) {
        let result = self.with_service(|service| -> Result<_> {
            // Validate the rule in the active document, including its policy.
            let outcome = service.generate()?;
            let mut document = outcome.config.as_value();
            if document["rules"].is_null() {
                document["rules"] = serde_json::json!([]);
            }
            let rules = document["rules"].as_array_mut().ok_or_else(|| {
                Error::invalid("rule", "the active configuration needs a rules list")
            })?;
            rules.insert(0, serde_json::Value::String(rule.to_owned()));
            let config = cvt_core::model::config::Config::from_value(document)?;
            let report = cvt_core::validate::check(&config);
            if !report.is_ok() {
                return Err(Error::Validation {
                    problems: report
                        .errors_iter()
                        .map(|finding| finding.message.clone())
                        .collect(),
                });
            }
            let mut store = service.store()?;
            let uid = store
                .current_uid()
                .ok_or_else(|| Error::invalid("rule", "select a base profile first"))?
                .to_owned();
            store.prepend_profile_rule(&uid, rule)?;
            Ok(())
        });
        Self::emit(
            sink,
            match result {
                Ok(()) => Event::Done(Done::ProfileContentChanged),
                Err(error) => Event::Failed(error.to_string()),
            },
        );
    }
}
