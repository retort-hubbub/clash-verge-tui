//! Build complete detail views for the selected item or screen.

use crate::action::Screen;
use crate::row::LogRow;

use super::{App, Effect, Overlay, StatusKind};

impl App {
    pub(super) fn copy_selection(&mut self) -> Vec<Effect> {
        let text = match &self.overlay {
            Some(Overlay::Message { text, .. }) => Some(text.clone()),
            Some(Overlay::Preview { lines, .. }) => Some(lines.join("\n")),
            Some(Overlay::Prompt { value, .. }) => Some(value.clone()),
            Some(Overlay::Confirm { question, .. }) => Some(self.tr(question).to_owned()),
            Some(Overlay::Picker {
                items, selected, ..
            }) => items.get(*selected).cloned(),
            None => match self.screen {
                Screen::Profiles => {
                    if let Some(row) = self.profiles.selected_item() {
                        if let Some(url) = &row.url {
                            Some(url.clone())
                        } else {
                            return vec![Effect::CopyProfile {
                                uid: row.uid.clone(),
                            }];
                        }
                    } else {
                        None
                    }
                }
                Screen::Proxies => {
                    if let Some(row) = self.nodes.selected_item() {
                        return vec![Effect::CopyProxy {
                            name: row.name.clone(),
                        }];
                    }
                    self.inspect_selection();
                    return self.copy_open_details();
                }
                Screen::Settings => self
                    .settings_rows
                    .selected_item()
                    .map(|row| row.editable.clone()),
                _ => {
                    self.inspect_selection();
                    return self.copy_open_details();
                }
            },
        };
        if let Some(text) = text.filter(|text| !text.is_empty()) {
            vec![Effect::CopyText { text }]
        } else {
            self.set_status(StatusKind::Info, self.tr("nothing to copy").to_owned());
            Vec::new()
        }
    }

    fn copy_open_details(&mut self) -> Vec<Effect> {
        if let Some(Overlay::Preview { .. } | Overlay::Message { .. }) = self.overlay {
            self.copy_selection()
        } else {
            self.set_status(StatusKind::Info, self.tr("nothing to copy").to_owned());
            Vec::new()
        }
    }

    pub(super) fn show_help_entry(&mut self) {
        if let Some(entry) = crate::ui::help::entries(self).get(self.help_selected) {
            self.overlay = Some(Overlay::Preview {
                title: "key reference".to_owned(),
                lines: vec![
                    format!("{} · {}", entry.group, entry.action),
                    format!("{} · {}", entry.keys, entry.context),
                    String::new(),
                    entry.description.clone(),
                ],
                scroll: 0,
            });
        }
    }

    pub(super) fn inspect_selection(&mut self) {
        if self.screen == Screen::Home {
            self.inspect_home();
            return;
        }
        if self.screen == Screen::Logs {
            self.inspect_latest_log();
            return;
        }
        if self.screen == Screen::Help {
            self.show_help_entry();
            return;
        }
        let fields: Option<Vec<(&str, String)>> = match self.screen {
            Screen::Profiles => self.profiles.selected_item().map(|row| {
                let mut fields = vec![
                    ("name", row.name.clone()),
                    ("uid", row.uid.clone()),
                    ("role", crate::i18n::profile_role(self.language(), row)),
                    (
                        "source",
                        row.url
                            .clone()
                            .unwrap_or_else(|| self.tr("local document").to_owned()),
                    ),
                    (
                        "updated",
                        row.updated_label(chrono::Local::now().timestamp()),
                    ),
                ];
                if let Some(base) = &row.base_scope {
                    fields.push(("belongs to", base.clone()));
                }
                if let Some(quota) = row.quota_label() {
                    fields.push(("remaining", quota));
                }
                if let Some(reason) = &row.unsupported {
                    fields.push(("cannot run", reason.clone()));
                }
                fields
            }),
            Screen::Proxies => self.nodes.selected_item().map(|row| {
                vec![
                    ("node", row.name.clone()),
                    ("group", row.group.clone().unwrap_or_default()),
                    ("type", row.kind.clone()),
                    ("delay", row.delay_label()),
                    (
                        "state",
                        self.tr(if row.active {
                            "active"
                        } else if row.alive {
                            "available"
                        } else {
                            "unavailable"
                        })
                        .to_owned(),
                    ),
                ]
            }),
            Screen::Connections => self.connections.selected_item().map(|row| {
                vec![
                    ("destination", row.destination.clone()),
                    ("process", row.process.clone()),
                    ("network", row.network.clone()),
                    ("rule", row.rule.clone()),
                    ("chain", row.chain.clone()),
                    ("transfer rate", self.connection_rate_label(&row.id)),
                    ("total traffic", row.traffic_label()),
                    ("id", row.id.clone()),
                ]
            }),
            Screen::Rules => self.rules.selected_item().map(|row| {
                vec![
                    ("rule", row.raw.clone()),
                    ("type", row.kind.clone()),
                    ("value", row.payload.clone()),
                    ("policy", row.policy.clone()),
                    (
                        "state",
                        self.tr(if row.disabled { "disabled" } else { "enabled" })
                            .to_owned(),
                    ),
                    ("hits", row.hits.to_string()),
                    ("misses", row.misses.to_string()),
                ]
            }),
            Screen::Tests => self.tests.selected_item().map(|row| {
                vec![
                    ("check", self.tr(row.kind.label()).to_owned()),
                    ("target", row.target.clone()),
                    ("what it does", self.tr(row.kind.description()).to_owned()),
                    ("result", row.result.label()),
                ]
            }),
            Screen::Settings => self.settings_rows.selected_item().map(|row| {
                vec![
                    ("setting", self.tr(row.label).to_owned()),
                    ("key", row.key.to_owned()),
                    ("value", row.value.clone()),
                    ("what it does", self.tr(row.help).to_owned()),
                ]
            }),
            Screen::Home | Screen::Logs | Screen::Help => None,
        };
        if let Some(fields) = fields {
            let lines = fields
                .into_iter()
                .map(|(label, value)| format!("{}: {value}", self.tr(label)))
                .collect();
            self.overlay = Some(Overlay::Preview {
                title: "full details".to_owned(),
                lines,
                scroll: 0,
            });
        } else {
            self.set_status(StatusKind::Info, "no selected row to inspect");
        }
    }

    pub(super) fn inspect_core_summary(&mut self) {
        self.overlay = Some(Overlay::Preview {
            title: "core".to_owned(),
            lines: vec![crate::ui::widgets::core_summary(self)],
            scroll: 0,
        });
    }

    pub(super) fn inspect_home(&mut self) {
        let mut lines = vec![
            format!(
                "{}: {}",
                self.tr("state"),
                crate::i18n::core_status(self.language(), &self.core)
            ),
            format!(
                "{}: {}",
                self.tr("mode"),
                self.core_mode
                    .as_deref()
                    .map_or_else(|| self.tr("unknown"), |mode| self.tr(mode))
            ),
            format!(
                "{}: {}",
                self.tr("version"),
                self.version
                    .as_deref()
                    .unwrap_or_else(|| self.tr("unknown"))
            ),
            format!("{}: {}", self.tr("data directory"), self.home.display()),
        ];
        if let Some(ip) = &self.ip_info {
            lines.extend([
                format!("IP: {}", ip.ip),
                format!("{}: {}", self.tr("country"), ip.country),
                format!("{}: {}", self.tr("network"), ip.organization),
            ]);
        }
        if let Some(error) = &self.ip_error {
            lines.push(format!("IP: {error}"));
        }
        self.overlay = Some(Overlay::Preview {
            title: "full details".to_owned(),
            lines,
            scroll: 0,
        });
    }

    pub(super) fn inspect_latest_log(&mut self) {
        if let Some(line) = self.log_window(1).0.last().copied().cloned() {
            self.show_log_detail(&line);
        } else {
            self.set_status(StatusKind::Info, "no log lines yet");
        }
    }

    pub(super) fn inspect_log_at(&mut self, column: u16, row: u16) {
        if let Some(line) = crate::ui::logs::line_at(self, column, row) {
            self.show_log_detail(&line);
        }
    }

    pub(super) fn show_log_detail(&mut self, line: &LogRow) {
        self.overlay = Some(Overlay::Preview {
            title: "full details".to_owned(),
            lines: vec![format!("{} {} {}", line.at, line.level, line.message)],
            scroll: 0,
        });
    }
}
