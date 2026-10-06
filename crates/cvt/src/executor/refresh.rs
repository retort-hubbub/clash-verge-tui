//! Refresh adapter for TUI effects.

use super::Executor;
use super::diagnostics::fetch_exit_ip;
use super::inventory::{config_node_rows, node_rows};
use cvt_core::Error;
use cvt_tui::row::{ConnectionRow, RuleRow};
use cvt_tui::{Data, Effect, Event, EventSink, Screen};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

impl Executor {
    // -------------------------------------------------------------- refresh

    /// A background read only reaches a controller while the core is running.
    /// Keep a configured but stopped controller quiet.
    pub(super) fn has_controller_endpoint(&self, sink: &EventSink) -> bool {
        if self.starting.load(Ordering::SeqCst) {
            return false;
        }
        match self.with_service(|service| {
            if !service.core_status().is_running() {
                return Ok(None);
            }
            service.endpoint()
        }) {
            Ok(Some(_)) => true,
            Ok(None) => false,
            Err(error) => {
                Self::emit(sink, Event::Failed(error.to_string()));
                false
            }
        }
    }

    /// Read whatever a screen shows.
    pub(super) fn refresh(&self, screen: Screen, sink: &EventSink) {
        match screen {
            Screen::Home => {
                self.start_streaming(sink);
                let status = self.with_service(|service| service.core_status());
                Self::emit(sink, Event::Data(Data::Core(status)));
                if self.has_controller_endpoint(sink) {
                    if let Some(address) = self.with_service(|service| service.proxy_addr()) {
                        let should_fetch = {
                            let mut last = self
                                .ip_last_fetch
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            if last.is_none_or(|time| time.elapsed() >= Duration::from_secs(60)) {
                                *last = Some(Instant::now());
                                true
                            } else {
                                false
                            }
                        };
                        if should_fetch {
                            Self::emit(sink, Event::Data(Data::IpLookupStarted));
                            let sink = sink.clone();
                            let current = Arc::clone(&self.ip_epoch);
                            let epoch = current.load(Ordering::SeqCst);
                            tokio::spawn(async move {
                                match fetch_exit_ip(&address).await {
                                    Ok(info) if current.load(Ordering::SeqCst) == epoch => {
                                        let _ = sink.send(Event::Data(Data::IpInfo(info)));
                                    }
                                    Err(error) if current.load(Ordering::SeqCst) == epoch => {
                                        let _ = sink.send(Event::Data(Data::IpLookupFailed(error)));
                                    }
                                    Ok(_) | Err(_) => {}
                                }
                            });
                        }
                    }
                    self.spawn_net(
                        sink,
                        |client| async move { client.version().await },
                        |version| Event::Data(Data::Version(version.trimmed().to_owned())),
                    );
                    self.spawn_net(
                        sink,
                        |client| async move { client.configs().await },
                        |config| {
                            Event::Data(Data::CoreMode(
                                config.mode.unwrap_or_else(|| "rule".to_owned()),
                            ))
                        },
                    );
                }
            }
            Screen::Logs => self.start_streaming(sink),
            Screen::Profiles => {
                let event = self
                    .local(Effect::LoadProfiles, sink)
                    .unwrap_or_else(|error| Event::Failed(error.to_string()));
                Self::emit(sink, event);
            }
            Screen::Settings => {
                let settings = self.with_service(|service| service.settings().clone());
                Self::emit(sink, Event::Data(Data::Settings(Box::new(settings))));
            }
            Screen::Proxies => {
                if self.has_controller_endpoint(sink) {
                    let group_order = self.with_service(|service| -> Result<Vec<String>, Error> {
                        let text = service.paths().read(&service.paths().runtime_config())?;
                        let config = cvt_core::model::config::Config::from_yaml(&text)?;
                        Ok(config
                            .proxy_groups()
                            .into_iter()
                            .map(|group| group.name)
                            .collect())
                    });
                    let group_order = match group_order {
                        Ok(order) => order,
                        Err(error) => {
                            Self::emit(
                                sink,
                                Event::Failed(format!(
                                    "cannot read generated proxy group order: {error}"
                                )),
                            );
                            return;
                        }
                    };
                    self.spawn_net(
                        sink,
                        |client| async move {
                            tokio::try_join!(client.proxies(), client.configs())
                        },
                        move |(inventory, config)| {
                            Event::Data(Data::Nodes(node_rows(
                                &inventory.proxies,
                                config.mode.as_deref(),
                                &group_order,
                            )))
                        },
                    );
                } else {
                    let rows = self.with_service(|service| {
                        service
                            .generate()
                            .ok()
                            .map_or_else(Vec::new, |outcome| config_node_rows(&outcome.config))
                    });
                    Self::emit(sink, Event::Data(Data::Nodes(rows)));
                }
            }
            Screen::Connections => {
                self.start_streaming(sink);
                if self.has_controller_endpoint(sink) {
                    self.spawn_net(
                        sink,
                        |client| async move { client.connections().await },
                        |response| {
                            let rows = response
                                .connections
                                .unwrap_or_default()
                                .iter()
                                .map(ConnectionRow::from_connection)
                                .collect();
                            Event::Data(Data::Connections(rows))
                        },
                    );
                } else {
                    Self::emit(sink, Event::Data(Data::Connections(Vec::new())));
                }
            }
            Screen::Rules => {
                if !self.has_controller_endpoint(sink) {
                    let rows = self.with_service(|service| -> cvt_core::Result<_> {
                        if service.store()?.current().is_none() {
                            return Ok(Vec::new());
                        }
                        let config = service.generate()?.config;
                        Ok(config
                            .rules()
                            .iter()
                            .enumerate()
                            .filter_map(|(index, rule)| {
                                u32::try_from(index)
                                    .ok()
                                    .map(|index| RuleRow::from_config_rule(rule, index))
                            })
                            .collect())
                    });
                    match rows {
                        Ok(rows) => Self::emit(sink, Event::Data(Data::Rules(rows))),
                        Err(error) => Self::emit(sink, Event::Failed(error.to_string())),
                    }

                    Self::emit(sink, Event::Data(Data::RuleProviders(Vec::new())));
                    return;
                }
                self.spawn_net(
                    sink,
                    |client| async move { client.rules().await },
                    |rules| {
                        Event::Data(Data::Rules(rules.iter().map(RuleRow::from_rule).collect()))
                    },
                );
                self.spawn_net(
                    sink,
                    |client| async move { client.rule_providers().await },
                    |providers| {
                        Event::Data(Data::RuleProviders(
                            providers.providers.into_keys().collect(),
                        ))
                    },
                );
            }
            // The tests screen is fed by its own results; help shows nothing.
            Screen::Tests | Screen::Help => {}
        }
    }
}
