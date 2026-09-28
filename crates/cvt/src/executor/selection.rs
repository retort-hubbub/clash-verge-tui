//! Selection effect handlers.

use super::Executor;
use cvt_tui::{Data, Done, Event, EventSink};
use std::sync::Arc;

impl Executor {
    pub(super) fn select_node(&self, group: String, member: String, sink: &EventSink) {
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
}

impl Executor {
    pub(super) fn clear_node_pin(&self, group: String, sink: &EventSink) {
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
}
