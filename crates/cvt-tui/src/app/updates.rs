//! Updates responsibilities of the application state machine.

use crate::action::{Action, Screen};
use crate::row::{ConnectionRow, NodeRow, ProbeMode, ProfileRow, RuleRow, TestKind};
use crate::state::SortOrder;
use crate::theme::Theme;

use super::{
    App, ConnectionSort, Data, Done, Effect, NODE_HEALTH_TTL, Overlay, SpeedMode, StatusKind,
    sort_group_members,
};
use cvt_core::settings::Settings;
use std::path::PathBuf;
use std::time::Instant;

impl App {
    // -- data arriving ------------------------------------------------------

    pub(super) fn on_data(&mut self, data: Data) -> Vec<Effect> {
        match data {
            Data::DnsConflict { conflict, next } => {
                if self.pending_confirmation.is_some() {
                    return Vec::new();
                }
                let question = crate::i18n::dns_conflict(self.language(), &conflict);
                self.pending_confirmation = Some(Effect::ResolveDnsConflict {
                    address: conflict.replacement,
                    next,
                });
                self.overlay = Some(Overlay::Confirm {
                    question,
                    action: Action::ResolveDnsConflict,
                });
            }
            Data::AppUpdateAvailable(release) => self.offer_app_update(release),
            Data::CoreAuthorized { next } => return vec![*next],
            Data::CoreAuthorization {
                binary,
                capabilities,
                next,
            } => {
                if self.pending_confirmation.is_some() {
                    // Repeated synchronization effects must not replace the
                    // user's in-progress authorization dialog.
                    return Vec::new();
                }
                let question = crate::i18n::message(
                    self.language(),
                    crate::i18n::Message::CoreAuthorization {
                        capabilities: &capabilities,
                        binary: &binary.to_string_lossy(),
                    },
                );
                self.pending_confirmation = Some(Effect::AuthorizeCore {
                    binary,
                    capabilities,
                    next,
                });
                self.overlay = Some(Overlay::Confirm {
                    question,
                    action: Action::AuthorizeCore,
                });
            }
            Data::Profiles(rows) => self.set_profiles(rows),
            Data::Nodes(rows) => self.set_nodes(rows),
            Data::Connections(rows) => {
                self.connection_traffic.update(&rows);
                self.set_connections(rows);
            }
            Data::Rules(rows) => self.set_rules(rows),
            Data::RuleProviders(names) => self.rule_providers = names,
            Data::Log(row) => self.logs.push(row),
            Data::Traffic(sample) => self.metrics.push_traffic(sample),
            Data::Memory(bytes) => self.metrics.push_memory(bytes),
            Data::Core(status) => {
                let was_running = self.core.is_running();
                self.core = status;
                if was_running && !self.core.is_running() {
                    // Nothing can be running against a core that is gone.
                    self.abandon_tests();
                    self.core_mode = None;
                    self.clear_ip();
                    self.route_speed = None;
                    self.invalidate_controller_views();
                    return vec![Effect::Refresh(Screen::Proxies)];
                }
                if !was_running && self.core.is_running() {
                    self.invalidate_controller_views();
                    return vec![
                        Effect::Refresh(Screen::Proxies),
                        Effect::Refresh(Screen::Rules),
                        Effect::Refresh(Screen::Connections),
                    ];
                }
            }
            Data::CoreMode(mode) => self.core_mode = Some(mode),
            Data::Version(version) => self.version = Some(version),
            Data::IpInfo(info) => {
                self.ip_info = Some(info);
                self.ip_error = None;
                self.ip_updated_at = Some(Instant::now());
                self.ip_refreshing = false;
            }
            Data::IpLookupStarted => self.ip_refreshing = true,
            Data::IpLookupFailed(error) => {
                self.set_status(StatusKind::Warning, format!("IP lookup failed: {error}"));
                self.ip_error = Some(error);
                self.ip_refreshing = false;
            }
            Data::RouteSpeed(result) => match result {
                Ok(value) => {
                    self.route_speed = Some(value.clone());
                    self.set_status(StatusKind::Success, format!("current route: {value}"));
                }
                Err(error) => self.set_status(StatusKind::Warning, format!("route speed: {error}")),
            },
            Data::SpeedtestMissing => {
                self.overlay = Some(Overlay::Confirm {
                    question: self.confirm_question(&Action::InstallSpeedtestGo),
                    action: Action::InstallSpeedtestGo,
                });
            }
            Data::Settings(settings) => self.set_settings(*settings),
            Data::Preview(preview) => {
                let lines = preview.lines();
                self.preview = Some(*preview);
                self.overlay = Some(Overlay::Preview {
                    title: "generated configuration".to_owned(),
                    lines,
                    scroll: 0,
                });
            }
            Data::ImportSources(sources) => return self.open_import_picker(&sources),
            Data::TestResult {
                mode,
                kind,
                target,
                result,
            } => {
                if mode == self.probe_mode || matches!(kind, TestKind::Unlock(_)) {
                    return self.on_test_result(kind, &target, result);
                }
            }
            Data::NodeDelay { mode, name, delay } => {
                if mode == ProbeMode::Connect {
                    self.node_health
                        .insert(name.clone(), (delay.is_some(), Instant::now()));
                }
                self.node_delays.insert((mode, name), delay);
                if mode == self.probe_mode || mode == ProbeMode::Connect {
                    self.rebuild_nodes();
                }
            }
            Data::Notice(text) => self.set_status(StatusKind::Info, text),
        }
        Vec::new()
    }

    pub(super) fn open_import_picker(&mut self, sources: &[PathBuf]) -> Vec<Effect> {
        if sources.is_empty() {
            self.set_status(
                StatusKind::Warning,
                "no clash-verge-rev installation was found to import from",
            );
            return Vec::new();
        }
        let items: Vec<String> = sources.iter().map(|p| p.display().to_string()).collect();
        self.overlay = Some(Overlay::Picker {
            title: "import from".to_owned(),
            items,
            selected: 0,
        });
        Vec::new()
    }

    pub(super) fn set_profiles(&mut self, rows: Vec<ProfileRow>) {
        let previous = self
            .profiles
            .selected_item()
            .map(|row| row.uid.clone())
            .or_else(|| {
                rows.iter()
                    .find(|row| row.current)
                    .map(|row| row.uid.clone())
            });
        let base = rows
            .iter()
            .find(|row| row.current)
            .map(|row| row.uid.clone());
        let chain: Vec<String> = rows
            .iter()
            .filter(|row| row.in_chain)
            .map(|row| row.uid.clone())
            .collect();
        if !chain.is_empty() || self.chain.is_empty() {
            self.chain = chain;
        }
        self.profiles.set_items(rows);
        if let Some(uid) = previous.or(base) {
            self.profiles.select_by_key(uid, |row| row.uid.clone());
        }
    }

    pub(super) fn set_nodes(&mut self, rows: Vec<NodeRow>) {
        self.all_nodes = rows;
        self.rebuild_nodes();
        self.refresh_test_targets();
    }

    pub(super) fn invalidate_controller_views(&mut self) {
        self.all_nodes.clear();
        self.node_delays.clear();
        self.node_health.clear();
        self.nodes.set_items(Vec::new());
        self.expanded.clear();
        self.connections.set_items(Vec::new());
        self.connection_traffic = super::connection_traffic::ConnectionTraffic::default();
        self.all_rules.clear();
        self.rules.set_items(Vec::new());
        self.rule_providers.clear();
        self.loaded.retain(|screen| {
            !matches!(
                screen,
                Screen::Proxies | Screen::Connections | Screen::Rules
            )
        });
    }

    /// Flatten the node tree to what the expansion state shows.
    ///
    /// Groups are collapsed the first time they are seen: a subscription can
    /// hold hundreds of nodes, and a list that starts as a wall of them hides
    /// the group the user is looking for.
    pub(super) fn rebuild_nodes(&mut self) {
        let selected = self
            .nodes
            .selected_item()
            .map(|row| (row.name.clone(), row.group.clone()));
        let expanded = self.expanded.clone();
        let mut rows: Vec<NodeRow> = self
            .all_nodes
            .iter()
            .filter(|row| {
                row.group
                    .as_deref()
                    .is_none_or(|group| expanded.iter().any(|e| e == group))
            })
            .cloned()
            .map(|mut row| {
                if !row.is_group
                    && let Some((alive, at)) = self.node_health.get(&row.name)
                    && at.elapsed() < NODE_HEALTH_TTL
                {
                    row.alive = *alive;
                }
                if self.probe_mode != ProbeMode::Connect {
                    row.delay = None;
                }
                if let Some(delay) = self.node_delays.get(&(self.probe_mode, row.name.clone())) {
                    row.delay = *delay;
                }
                row
            })
            .collect();
        sort_group_members(&mut rows, self.node_sort);
        self.nodes.set_items(rows);
        if let Some(key) = selected {
            self.nodes
                .select_by_key(key, |row| (row.name.clone(), row.group.clone()));
        }
    }

    /// Order members inside each group without moving a group heading.
    pub(super) fn sort_nodes(&mut self, order: SortOrder) {
        self.node_sort = order;
        self.rebuild_nodes();
    }

    pub(super) fn set_connections(&mut self, rows: Vec<ConnectionRow>) {
        let previous = self.connections.selected_item().map(|row| row.id.clone());
        let count = rows.len();
        self.connections.set_items(rows);
        if let Some(id) = previous {
            self.connections.select_by_key(id, |row| row.id.clone());
        }
        self.metrics.set_connections(count);
        self.resort_connections();
        self.refresh_test_targets();
    }

    /// Keep the tests screen pointed at what the other screens now show.
    pub(super) fn refresh_test_targets(&mut self) {
        if self.screen == Screen::Tests {
            self.rebuild_tests();
        }
    }

    pub(super) fn resort_connections(&mut self) {
        let order = self.connection_sort;
        if order == ConnectionSort::Natural {
            return;
        }
        let selected = self.connections.selected_item().map(|row| row.id.clone());
        let mut rows = self.connections.items().to_vec();
        match order {
            ConnectionSort::Busiest => rows.sort_by_key(|row| std::cmp::Reverse(row.total())),
            ConnectionSort::Fastest => {
                rows.sort_by_key(|row| std::cmp::Reverse(self.connection_rate_total(&row.id)));
            }
            ConnectionSort::Oldest => rows.sort_by(|a, b| a.started.cmp(&b.started)),
            ConnectionSort::Newest => rows.sort_by(|a, b| b.started.cmp(&a.started)),
            ConnectionSort::Natural => {}
        }
        self.connections.set_items(rows);
        if let Some(id) = selected {
            self.connections.select_by_key(id, |row| row.id.clone());
        }
    }

    pub(super) fn set_rules(&mut self, rows: Vec<RuleRow>) {
        self.all_rules = rows;
        self.rebuild_rules();
    }

    /// Show the rules that are currently relevant.
    pub(super) fn rebuild_rules(&mut self) {
        let selected = self.rules.selected_item().map(|row| row.index);
        let show_disabled = self.show_disabled_rules;
        let rows: Vec<RuleRow> = self
            .all_rules
            .iter()
            .filter(|row| show_disabled || !row.disabled)
            .cloned()
            .collect();
        self.rules.set_items(rows);
        if let Some(index) = selected {
            self.rules.select_by_key(index, |row| row.index);
        }
    }

    pub(super) fn set_settings(&mut self, settings: Settings) {
        // The disk's copy — unless the user has edits it would throw away.
        //
        // Replacing them and clearing the dirty flag made the interface report
        // a save that wrote nothing: the screen showed the edit, `s` said
        // "settings saved", and what reached the file was the copy from disk.
        // Nothing on the screen said the edit was gone, which is the part that
        // makes it a defect rather than a policy.
        //
        // Keeping the user's copy is the priority: unsaved work is the thing
        // that cannot be recovered by looking again.
        if self.settings_dirty {
            self.set_status(
                StatusKind::Warning,
                "the settings on disk changed; your edits are kept — `s` writes them",
            );
            return;
        }
        self.log_level = settings.ui.log_level;
        self.theme = Theme::from_settings(settings.ui.color && self.theme.color);
        self.settings = settings;
        self.settings_dirty = false;
        self.rebuild_settings_rows();
    }

    // -- operations finishing -----------------------------------------------

    pub(super) fn on_done(&mut self, done: Done) -> Vec<Effect> {
        match done {
            Done::ClipboardCopied { terminal } => {
                let message = if terminal {
                    "clipboard request sent; your terminal must allow OSC 52"
                } else {
                    "copied to clipboard"
                };
                self.set_status(StatusKind::Success, self.tr(message).to_owned());
            }
            Done::AppUpdated { version, backup } => {
                let label = self.tr("application updated; restart to use the new version");
                self.set_status(
                    StatusKind::Success,
                    format!(
                        "{label}: {version}; {} {}",
                        self.tr("previous executable:"),
                        backup.display()
                    ),
                );
            }
            Done::ProfilesLoaded => self.set_status(StatusKind::Success, "profiles loaded"),
            Done::ProfileSwitched { name } => {
                self.clear_ip();
                self.route_speed = None;
                self.set_status(StatusKind::Success, format!("switched to `{name}`"));
                self.invalidate_controller_views();
                return vec![
                    Effect::LoadProfiles,
                    Effect::Refresh(Screen::Home),
                    Effect::Refresh(Screen::Proxies),
                    Effect::Refresh(Screen::Rules),
                    Effect::Refresh(Screen::Connections),
                ];
            }
            Done::ProfileContentChanged => {
                return vec![Effect::LoadProfiles, Effect::SynchronizeConfig];
            }
            Done::ProfilesUpdated { updated, failed } => {
                let kind = if failed == 0 {
                    StatusKind::Success
                } else {
                    StatusKind::Warning
                };
                self.set_status(
                    kind,
                    format!("{updated} profile(s) updated, {failed} failed"),
                );
                return vec![Effect::LoadProfiles];
            }
            Done::ChainSaved => self.set_status(StatusKind::Success, "chain saved"),
            Done::ConfigApplied { reload, changed } => {
                self.clear_ip();
                self.route_speed = None;
                match reload {
                    Some(outcome) if outcome.succeeded() => self.set_status(
                        StatusKind::Success,
                        format!("{} ({changed} change(s))", outcome.summary()),
                    ),
                    Some(outcome) => {
                        self.set_status(StatusKind::Error, outcome.summary());
                    }
                    None => self.set_status(
                        StatusKind::Success,
                        format!(
                            "configuration written ({changed} change(s)); the core is not running"
                        ),
                    ),
                }
                return vec![
                    Effect::Refresh(Screen::Home),
                    Effect::Refresh(Screen::Proxies),
                    Effect::Refresh(Screen::Rules),
                    Effect::Refresh(Screen::Connections),
                ];
            }
            Done::ConfigRolledBack { snapshot } => self.set_status(
                StatusKind::Warning,
                format!("restored {}", snapshot.display()),
            ),
            Done::ProfileDeleted { name, was_current } => {
                self.loaded.retain(|&s| s != Screen::Profiles);
                self.set_status(StatusKind::Success, format!("deleted `{name}`"));
                if was_current {
                    self.invalidate_controller_views();
                    if self.core.is_running() {
                        return vec![Effect::LoadProfiles, Effect::StopCore];
                    }
                    return vec![Effect::LoadProfiles, Effect::Refresh(Screen::Proxies)];
                }
                return vec![Effect::LoadProfiles];
            }
            Done::ProfileRenamed { name } => {
                self.loaded.retain(|&s| s != Screen::Profiles);
                self.set_status(StatusKind::Success, format!("renamed to `{name}`"));
                return vec![Effect::LoadProfiles];
            }
            Done::ProfileCreated {
                name,
                uid,
                is_remote,
            } => {
                self.loaded.retain(|&s| s != Screen::Profiles);
                self.set_status(StatusKind::Success, format!("created `{name}`"));
                let mut effects = vec![Effect::LoadProfiles];
                if is_remote && let Some(uid) = uid {
                    effects.push(Effect::UpdateProfiles { uids: vec![uid] });
                }
                return effects;
            }
            Done::ProfilesImported { count } => {
                self.loaded.retain(|&s| s != Screen::Profiles);
                self.set_status(StatusKind::Success, format!("imported {count} profile(s)"));
                return vec![Effect::LoadProfiles];
            }
            Done::NodeSelected { group, member } => {
                self.clear_ip();
                self.route_speed = None;
                for row in &mut self.all_nodes {
                    if row.group.as_deref() == Some(group.as_str()) {
                        row.active = row.name == member;
                    }
                }
                self.rebuild_nodes();
                self.set_status(
                    StatusKind::Success,
                    format!("`{group}` now uses `{member}`"),
                );
                return vec![Effect::Refresh(Screen::Proxies), Effect::RefreshIp];
            }
            Done::NodeCleared { group } => {
                self.clear_ip();
                self.route_speed = None;
                for row in &mut self.all_nodes {
                    if row.group.as_deref() == Some(group.as_str()) {
                        row.active = false;
                    }
                }
                self.rebuild_nodes();
                self.set_status(
                    StatusKind::Success,
                    format!("`{group}` chooses automatically again"),
                );
                return vec![Effect::Refresh(Screen::Proxies), Effect::RefreshIp];
            }
            Done::NodeTestsFinished { tested } => {
                self.set_status(StatusKind::Success, format!("measured {tested} node(s)"));
            }
            Done::ConnectionClosed { id } => {
                let remaining = self
                    .connections
                    .items()
                    .iter()
                    .filter(|row| row.id != id)
                    .cloned()
                    .collect();
                self.set_connections(remaining);
                self.set_status(StatusKind::Success, "connection closed");
                return vec![Effect::Refresh(Screen::Connections)];
            }
            Done::ConnectionsClosed { count } => {
                self.set_connections(Vec::new());
                self.set_status(StatusKind::Success, format!("closed {count} connection(s)"));
                return vec![Effect::Refresh(Screen::Connections)];
            }
            Done::RuleToggled { index, disabled } => {
                if let Some(rule) = self.all_rules.iter_mut().find(|rule| rule.index == index) {
                    rule.disabled = disabled;
                }
                self.rebuild_rules();
                let state = if disabled { "disabled" } else { "enabled" };
                self.set_status(StatusKind::Success, format!("rule {index} {state}"));
                return vec![Effect::Refresh(Screen::Rules)];
            }
            Done::RuleProvidersUpdated { count } => {
                self.set_status(StatusKind::Success, format!("updated {count} rule set(s)"));
            }
            Done::CoreStarted { pid } => {
                self.set_status(StatusKind::Success, format!("core started (pid {pid})"));
                return vec![
                    Effect::Refresh(Screen::Home),
                    Effect::Refresh(Screen::Proxies),
                ];
            }
            Done::CoreStopped => {
                self.set_status(StatusKind::Success, "core stopped");
                self.invalidate_controller_views();
                return vec![
                    Effect::Refresh(Screen::Home),
                    Effect::Refresh(Screen::Proxies),
                ];
            }
            Done::CoreRestarted { pid } => {
                self.set_status(StatusKind::Success, format!("core restarted (pid {pid})"));
                return vec![
                    Effect::Refresh(Screen::Home),
                    Effect::Refresh(Screen::Proxies),
                ];
            }
            Done::CoreModeChanged { mode } => {
                self.clear_ip();
                self.route_speed = None;
                self.core_mode = Some(mode.clone());
                self.set_status(StatusKind::Success, format!("routing mode: {mode}"));
                return vec![
                    Effect::Refresh(Screen::Proxies),
                    Effect::Refresh(Screen::Home),
                ];
            }
            Done::CoreUpgraded { version } => {
                self.set_status(
                    StatusKind::Success,
                    format!("installed mihomo {version} (managed)"),
                );
                return vec![Effect::Refresh(Screen::Home)];
            }
            Done::SpeedtestInstalled { version } => {
                self.set_status(
                    StatusKind::Success,
                    format!("installed speedtest-go {version}"),
                );
                if self.core.is_running() && self.speed_mode == SpeedMode::Speedtest {
                    return vec![Effect::TestRouteSpeed {
                        mode: SpeedMode::Speedtest,
                    }];
                }
            }
            Done::GeoUpdated => self.set_status(StatusKind::Success, "geo databases updated"),
            Done::CachesFlushed => self.set_status(StatusKind::Success, "caches flushed"),
            Done::SettingsSaved => {
                self.settings_dirty = false;
                if self
                    .settings
                    .core
                    .secret
                    .as_deref()
                    .is_none_or(str::is_empty)
                {
                    self.set_status(StatusKind::Warning, self.tr("settings saved; controller secret is empty: API access is unauthenticated").to_owned());
                } else {
                    self.set_status(StatusKind::Success, "settings saved");
                }
            }
            Done::LogsExported { path } => self.set_status(
                StatusKind::Success,
                format!("logs written to {}", path.display()),
            ),
            Done::EditorOpened { target } => {
                self.set_status(StatusKind::Info, format!("opened {target} in $EDITOR"));
            }
        }
        Vec::new()
    }
}
