//! Actions responsibilities of the application state machine.

use super::{App, Effect, Overlay, PromptKind, Rows, SpeedMode, StatusKind};
use crate::action::{Action, Screen};
use crate::state::SortOrder;

impl App {
    // -- action dispatch ----------------------------------------------------

    pub(super) fn dispatch(&mut self, action: Action, confirmed: bool) -> Vec<Effect> {
        if action == Action::SaveSettings
            && !confirmed
            && self.settings.core.tun_enabled == Some(true)
            && self.settings_dirty
        {
            self.overlay = Some(Overlay::Confirm {
                question: "enable TUN and grant the core network capabilities? this may change system routes; the grant persists on the core binary".to_owned(),
                action,
            });
            return Vec::new();
        }
        if action.is_destructive() && !confirmed {
            if let Some(reason) = self.destructive_blocker(&action) {
                self.refuse(reason);
                return Vec::new();
            }
            let question = self.confirm_question(&action);
            self.overlay = Some(Overlay::Confirm { question, action });
            return Vec::new();
        }
        self.perform(&action)
    }

    /// Check whether a destructive action has a target before confirming it.
    pub(super) fn destructive_blocker(&self, action: &Action) -> Option<String> {
        match action {
            Action::StopCore if !self.core.is_running() => {
                Some("the core is not running".to_owned())
            }
            Action::DeleteProfile if self.profiles.is_empty() => {
                Some("there is no profile to delete".to_owned())
            }
            Action::CloseAllConnections if self.connections.is_empty() => {
                Some("there is no connection to close".to_owned())
            }
            _ => None,
        }
    }

    pub(super) fn confirm_question(&self, action: &Action) -> String {
        match action {
            Action::DeleteProfile => self.profiles.selected_item().map_or_else(
                || "delete this profile?".to_owned(),
                |row| format!("delete `{}` and its document?", row.name),
            ),
            Action::CloseAllConnections => {
                format!("close all {} connection(s)?", self.connections.total())
            }
            Action::StopCore => "stop the core?".to_owned(),
            Action::RollbackConfig => {
                "restore the previous generated configuration and restart the core?".to_owned()
            }
            Action::UpgradeCore => self
                .tr_key(crate::i18n::TextKey::ManagedCoreConfirmation)
                .to_owned(),
            Action::InstallSpeedtestGo => {
                "download and install speedtest-go in the application directory?".to_owned()
            }
            other => format!("{}?", other.label()),
        }
    }

    /// Dispatch an action using the current application state.
    #[allow(clippy::too_many_lines)] // one arm per action is the readable form
    pub(super) fn perform(&mut self, action: &Action) -> Vec<Effect> {
        match action {
            Action::Quit => {
                self.quit = true;
                vec![Effect::Quit]
            }
            Action::Goto(screen) => self.goto(*screen),
            Action::NextScreen => self.goto(self.screen.next()),
            Action::PreviousScreen => self.goto(self.screen.previous()),
            Action::Refresh => {
                self.remember_loaded(self.screen);
                if self.screen == Screen::Home {
                    vec![Effect::RefreshIp]
                } else {
                    Self::refresh_effects(self.screen)
                }
            }
            Action::Cancel => {
                self.cancel_scope();
                Vec::new()
            }
            Action::CopySelection => self.copy_selection(),
            Action::ShowLastMessage => self.show_last_message(),
            Action::InspectSelection => {
                self.inspect_selection();
                Vec::new()
            }
            Action::Up => self.move_cursor(-1),
            Action::Down => self.move_cursor(1),
            Action::PageUp => self.page_cursor(-1),
            Action::PageDown => self.page_cursor(1),
            Action::Top => self.move_to_edge(true),
            Action::Bottom => self.move_to_edge(false),
            Action::Search => {
                let current = self.active_rows().map_or_else(String::new, Rows::filter);
                self.open_prompt(PromptKind::Search, current);
                Vec::new()
            }
            Action::SearchNext => {
                if let Some(rows) = self.active_rows_mut() {
                    rows.next_match();
                }
                self.sync_scroll();
                Vec::new()
            }
            Action::ActivateProfile => self.activate_profile(),
            Action::UpdateProfile => self.update_profile(),
            // The executor owns the full index and due calculation; an empty
            // list requests all due profiles, including rows not yet loaded.
            Action::UpdateAllProfiles => vec![Effect::UpdateProfiles { uids: Vec::new() }],
            Action::NewProfile => {
                self.overlay = Some(Overlay::Picker {
                    title: "new profile".to_owned(),
                    items: vec!["from a URL".to_owned(), "a blank local profile".to_owned()],
                    selected: 0,
                });
                Vec::new()
            }
            Action::DeleteProfile => self.delete_profile(),
            Action::RenameProfile => {
                let Some(row) = self.profiles.selected_item() else {
                    self.refuse("no profile selected");
                    return Vec::new();
                };
                let name = row.name.clone();
                self.open_prompt(PromptKind::Rename, name);
                Vec::new()
            }
            Action::ImportProfiles => vec![Effect::DetectImportSources],
            Action::EditProfile => self.edit_profile(),
            Action::EditProfileSource => {
                let row = if self.screen == Screen::Profiles {
                    self.profiles.selected_item()
                } else {
                    self.profiles.items().iter().find(|row| row.current)
                };
                let Some(row) = row else {
                    self.refuse("no profile selected");
                    return Vec::new();
                };
                let Some(url) = row.url.clone() else {
                    self.refuse("a local profile has no subscription URL");
                    return Vec::new();
                };
                self.pending_profile_source = Some(row.uid.clone());
                self.open_prompt(PromptKind::ProfileUrl, url);
                Vec::new()
            }
            Action::EditProfileOverride => {
                let uid = if self.screen == Screen::Profiles {
                    let Some(row) = self.profiles.selected_item() else {
                        self.refuse("no profile selected");
                        return Vec::new();
                    };
                    if !row.kind.is_base() {
                        self.refuse("select a base profile first");
                        return Vec::new();
                    }
                    Some(row.uid.clone())
                } else {
                    None
                };
                vec![Effect::EditProfileOverride { uid }]
            }
            Action::AddRule => {
                self.open_prompt(PromptKind::Rule, String::new());
                Vec::new()
            }
            Action::AuthorizeCore | Action::ResolveDnsConflict => {
                self.pending_confirmation.take().into_iter().collect()
            }
            Action::ToggleInChain => self.toggle_in_chain(),
            Action::PreviewConfig => vec![Effect::PreviewConfig],
            Action::ApplyConfig => vec![Effect::ApplyConfig {
                mode: self.reload_mode(),
            }],
            Action::RollbackConfig => vec![Effect::RollbackConfig],
            Action::SelectNode => self.select_node(),
            Action::TestGroup => self.test_group(),
            Action::TestNode => self.test_node(),
            Action::CycleTestMode => {
                self.probe_mode = self.probe_mode.next();
                self.node_delays.clear();
                self.node_sort = SortOrder::Natural;
                self.rebuild_nodes();
                self.set_status(
                    StatusKind::Info,
                    format!("{}: {}", self.tr("test mode"), self.probe_mode.label()),
                );
                vec![Effect::Refresh(Screen::Proxies)]
            }
            Action::TestRouteSpeed => {
                if !self.require_core("measuring route speed") {
                    return Vec::new();
                }
                self.overlay = Some(Overlay::Picker {
                    title: "route bandwidth".to_owned(),
                    items: [
                        SpeedMode::Sample4,
                        SpeedMode::Sample20,
                        SpeedMode::Sample100,
                        SpeedMode::Speedtest,
                    ]
                    .map(|mode| mode.label().to_owned())
                    .to_vec(),
                    selected: self.speed_mode.index(),
                });
                Vec::new()
            }
            Action::InstallSpeedtestGo => {
                self.set_status(StatusKind::Info, "downloading speedtest-go…");
                vec![Effect::InstallSpeedtestGo]
            }
            Action::TestAllNodes => {
                if !self.require_core("testing nodes") {
                    return Vec::new();
                }
                vec![Effect::TestAllNodes {
                    mode: self.probe_mode,
                }]
            }
            Action::ClearNodeSelection => self.clear_node_pin(),
            Action::CloseConnection => self.close_connection(),
            Action::CloseAllConnections => {
                if !self.require_core("closing connections") {
                    return Vec::new();
                }
                vec![Effect::CloseAllConnections]
            }
            Action::CycleConnectionSort => {
                self.connection_sort = self.connection_sort.next();
                self.resort_connections();
                let label = self.connection_sort.label();
                self.set_status(StatusKind::Info, format!("connections sorted by {label}"));
                Vec::new()
            }
            Action::CycleNodeSort => {
                let next = match self.node_sort {
                    SortOrder::Natural | SortOrder::TrafficDescending => {
                        SortOrder::LatencyAscending
                    }
                    SortOrder::LatencyAscending => SortOrder::LatencyDescending,
                    SortOrder::LatencyDescending => SortOrder::Natural,
                };
                self.sort_nodes(next);
                self.set_status(
                    StatusKind::Info,
                    format!("members sorted by {}", next.label()),
                );
                Vec::new()
            }
            Action::ToggleLogFollow => {
                self.logs.follow = !self.logs.follow;
                self.frozen = if self.logs.follow {
                    None
                } else {
                    Some(self.logs.filtered().len())
                };
                let text = if self.logs.follow {
                    "following new lines"
                } else {
                    "follow stopped; new lines are buffered but not shown"
                };
                self.set_status(StatusKind::Info, text);
                Vec::new()
            }
            Action::CycleLogLevel => self.cycle_log_level(),
            Action::ClearLogs => {
                let dropped = self.logs.len();
                self.logs.clear();
                self.set_status(StatusKind::Info, format!("discarded {dropped} log line(s)"));
                Vec::new()
            }
            Action::ExportLogs => {
                if self.logs.is_empty() {
                    self.refuse("the log buffer is empty");
                    return Vec::new();
                }
                let path = self.log_export_path();
                let contents = self.logs.export();
                vec![Effect::ExportLogs { path, contents }]
            }
            Action::ToggleRule => self.toggle_rule(),
            Action::UpdateRuleProvider => self.update_rule_provider(),
            Action::UpdateAllRuleProviders => {
                if !self.require_core("updating rule sets") {
                    return Vec::new();
                }
                vec![Effect::UpdateRuleProviders { names: Vec::new() }]
            }
            Action::ToggleDisabledRules => {
                self.show_disabled_rules = !self.show_disabled_rules;
                self.rebuild_rules();
                let text = if self.show_disabled_rules {
                    "showing disabled rules"
                } else {
                    "hiding disabled rules"
                };
                self.set_status(StatusKind::Info, text);
                Vec::new()
            }
            Action::RunTests => self.run_tests(),
            Action::RunAllTests => self.run_all_tests(),
            Action::CancelTests => self.cancel_tests(),
            Action::ClearTestResults => {
                self.clear_test_results();
                vec![Effect::ClearTestResults]
            }
            Action::StartCore => vec![Effect::StartCore],
            Action::StopCore => vec![Effect::StopCore],
            Action::RestartCore => {
                if !self.require_core("restarting the core") {
                    return Vec::new();
                }
                vec![Effect::RestartCore]
            }
            Action::CycleCoreMode => {
                if !self.require_core("changing the routing mode") {
                    return Vec::new();
                }
                let next = match self.core_mode.as_deref() {
                    Some("rule") => "global",
                    Some("global") => "direct",
                    _ => "rule",
                };
                vec![Effect::SetCoreMode {
                    mode: next.to_owned(),
                }]
            }
            Action::UpgradeCore => vec![Effect::UpgradeCore],
            Action::CheckAppUpdate => vec![Effect::CheckAppUpdate { manual: true }],
            Action::UpdateGeo => {
                if !self.require_core("updating geo databases") {
                    return Vec::new();
                }
                vec![Effect::UpdateGeo]
            }
            Action::FlushCaches => {
                if !self.require_core("flushing caches") {
                    return Vec::new();
                }
                vec![Effect::FlushCaches]
            }
            Action::EditRuntimeConfig => vec![Effect::OpenEditor {
                path: self.runtime_config_path(),
            }],
            Action::SaveSettings => vec![Effect::SaveSettings {
                settings: self.settings.clone(),
            }],
            Action::ToggleSetting => self.toggle_setting(),
        }
    }
}
