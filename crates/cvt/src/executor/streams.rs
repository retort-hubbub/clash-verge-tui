//! Streams adapter for TUI effects.

use super::Executor;
use cvt_core::mihomo::stream::{Event as StreamEvent, Options, Selection, Stream};
use cvt_tui::row::{ConnectionRow, LogRow};
use cvt_tui::{Data, Event, EventSink};
use std::sync::Arc;
use std::sync::atomic::Ordering;

impl Executor {
    // ---------------------------------------------------------- live stream

    /// Start the live stream, once.
    ///
    /// Traffic, memory and logs arrive on one connection and keep arriving, so
    /// this starts on the first refresh and is then left alone: starting it
    /// again on every tick would open a new WebSocket every refresh interval.
    pub(super) fn start_streaming(&self, sink: &EventSink) {
        if self.starting.load(Ordering::SeqCst) {
            return;
        }
        let (level, endpoint) = self.with_service(|service| {
            (
                service.settings().ui.log_level,
                service
                    .core_status()
                    .is_running()
                    .then(|| service.endpoint().ok().flatten())
                    .flatten(),
            )
        });
        let Some(endpoint) = endpoint else {
            // Not worth a status line: the interface works without a running
            // core, it simply has nothing live to show.
            tracing::debug!("no core endpoint yet, so no live stream");
            return;
        };
        if self.streaming.swap(true, Ordering::SeqCst) {
            return;
        }

        let sink = sink.clone();
        let streaming = Arc::clone(&self.streaming);
        tokio::spawn(async move {
            let options = Options::new(Selection {
                traffic: true,
                memory: true,
                logs: true,
                connections: false,
            })
            .with_log_level(level);

            let mut stream = match Stream::spawn(endpoint, options) {
                Ok(stream) => stream,
                Err(error) => {
                    let _ = sink.send(Event::Failed(error.to_string()));
                    streaming.store(false, Ordering::SeqCst);
                    return;
                }
            };
            while let Some(event) = stream.recv().await {
                let event = match event {
                    StreamEvent::Traffic(traffic) => Event::Data(Data::Traffic(traffic)),
                    StreamEvent::Memory(memory) => Event::Data(Data::Memory(memory.inuse)),
                    StreamEvent::Log(log) => Event::Data(Data::Log(LogRow::from_event(&log))),
                    StreamEvent::Connections(response) => {
                        let rows = response
                            .connections
                            .unwrap_or_default()
                            .iter()
                            .map(ConnectionRow::from_connection)
                            .collect();
                        Event::Data(Data::Connections(rows))
                    }
                    StreamEvent::Dropped { count } => {
                        Event::Data(Data::Notice(format!("{count} events dropped")))
                    }
                    StreamEvent::Opened { .. } | StreamEvent::Closed { .. } => continue,
                };
                // A closed sink means the interface is gone, and so is the
                // reason to keep this connection open.
                if !sink.send(event) {
                    break;
                }
            }
            streaming.store(false, Ordering::SeqCst);
        });
    }
}
