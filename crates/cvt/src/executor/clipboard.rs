//! Resolve complete copy payloads at the application I/O boundary.

use super::{Done, Error, Event, EventSink, Executor};

impl Executor {
    pub(super) fn copy_text(text: String, sink: &EventSink) {
        let sink = sink.clone();
        tokio::spawn(async move {
            let event = match crate::clipboard::copy(&text).await {
                Ok(terminal) => Event::Done(Done::ClipboardCopied { terminal }),
                Err(error) => Event::Failed(error.to_string()),
            };
            Self::emit(&sink, event);
        });
    }

    pub(super) fn copy_profile(&self, uid: &str, sink: &EventSink) {
        let result = self.with_service(|service| {
            let store = service.store()?;
            let item = store.get(uid).ok_or_else(|| Error::ProfileNotFound {
                uid: uid.to_owned(),
            })?;
            store.read_document(item)
        });
        match result {
            Ok(text) => Self::copy_text(text, sink),
            Err(error) => Self::emit(sink, Event::Failed(error.to_string())),
        }
    }

    pub(super) fn copy_proxy(&self, name: &str, sink: &EventSink) {
        let result = self.with_service(|service| {
            let outcome = service.generate()?;
            let node = ["proxies", "proxy-groups"]
                .iter()
                .find_map(|key| {
                    outcome
                        .config
                        .get(key)
                        .and_then(serde_json::Value::as_array)
                        .and_then(|nodes| {
                            nodes.iter().find(|node| {
                                node.get("name").and_then(serde_json::Value::as_str) == Some(name)
                            })
                        })
                })
                .ok_or_else(|| {
                    Error::invalid("copy", "the selected node has no outbound definition")
                })?;
            serde_norway::to_string(node)
                .map_err(|error| Error::serialize("proxy configuration", error))
        });
        match result {
            Ok(text) => Self::copy_text(text, sink),
            Err(error) => Self::emit(sink, Event::Failed(error.to_string())),
        }
    }
}
