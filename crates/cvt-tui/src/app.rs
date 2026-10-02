//! The application state machine.
//!
//! Three types carry the whole protocol between the interface and the world:
//! a key press becomes an [`Action`](crate::action::Action) (or a mouse event changes the current
//! selection), the machine answers with a list of
//! [`Effect`]s, and the binary crate performs them and reports back with an
//! [`Event`]. State transitions perform no filesystem, network or process I/O.
//! Construct with [`App::with_settings`] to inject settings already loaded by
//! the caller. [`App::new`] remains a convenience loader for library callers.
//!
//! ```text
//! KeyEvent ──▶ Keymap ──▶ Action ──▶ App ──▶ Vec<Effect> ──▶ binary
//! MouseEvent ────────────────────────┘
//!                                       ◀── Event::{Data,Done,Failed}
//! ```
//!
//! # Where the decisions live
//!
//! [`App::on_event`] is the only entry point. It routes input through the
//! overlay first (so a prompt can be typed into), then keys through the key
//! map and dispatcher. The dispatcher is deliberately explicit about every
//! [`Action`](crate::action::Action): an arm that cannot do what was asked reports *why* through
//! [`StatusKind::Warning`] instead of failing silently, because "the key did
//! nothing" is the worst answer a keyboard-driven program can give.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use cvt_core::ReloadMode;
use cvt_core::mihomo::supervisor::CoreStatus;
use cvt_core::mihomo::types::LogLevel;
use cvt_core::settings::{Language, Settings};

use crate::action::Screen;
use crate::keys::Keymap;
use crate::row::{ConnectionRow, LogRow, NodeRow, ProbeMode, ProfileRow, RuleRow, TestRow};
use crate::state::{LogBuffer, Metrics, SortOrder, Table};
use crate::theme::Theme;

mod actions;
mod application_update;
mod details;
mod input;
mod lists;
mod navigation;
mod overlay;
mod profiles;
mod protocol;
mod proxies;
mod settings;
mod testing;
mod updates;

pub(crate) use application_update::UPDATE_TITLE;
pub use navigation::ConnectionSort;
use navigation::Rows;
pub use overlay::{Overlay, PromptKind, Status, StatusKind};
pub use protocol::{
    Data, Done, Effect, Event, IpInfo, Preview, PreviewChange, PreviewFinding, SpeedMode,
    UpdateRelease,
};
pub use settings::{SettingKind, SettingRow, setting_rows};
use settings::{set_setting_choice, set_setting_text};

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests;

/// How long a transient status message stays on screen.
///
/// Long enough to read a sentence, short enough that it does not keep covering
/// the footer. The full latest message remains available through `m`.
pub const STATUS_TTL: Duration = Duration::from_secs(4);

/// How many ticks pass between background polls of the running core.
pub const POLL_EVERY: u64 = 20;
const NODE_HEALTH_TTL: Duration = Duration::from_secs(300);
const DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(500);

// ---------------------------------------------------------------------------
// the application
// ---------------------------------------------------------------------------

/// All the interface state, and the rules for changing it.
#[derive(Debug)]
pub struct App {
    /// Application home; the only path prefix the interface needs.
    pub home: PathBuf,
    /// The palette, rebuilt when the colour setting changes.
    pub theme: Theme,
    /// The screen on show.
    pub screen: Screen,
    /// The key map, consulted for every key press.
    pub keymap: Keymap,
    /// The modal layer on top, which consumes keys first.
    pub overlay: Option<Overlay>,
    pending_authorization: Option<Effect>,
    pending_profile_source: Option<String>,
    /// Latest application release waiting for a safe modal opportunity.
    pub app_update: Option<UpdateRelease>,
    app_update_next_check: Instant,
    /// Profiles, as the store lists them.
    pub profiles: Table<ProfileRow>,
    /// Proxy rows, flattened to what is currently visible.
    pub nodes: Table<NodeRow>,
    /// The connection table.
    pub connections: Table<ConnectionRow>,
    /// The rule table, excluding disabled rules unless asked for.
    pub rules: Table<RuleRow>,
    /// The tests screen.
    pub tests: Table<TestRow>,
    /// The settings screen.
    pub settings_rows: Table<SettingRow>,
    /// Buffered log lines.
    pub logs: LogBuffer,
    /// Rolling traffic and memory gauges.
    pub metrics: Metrics,
    /// What the supervisor last reported.
    pub core: CoreStatus,
    /// Routing mode reported by Mihomo's live configuration.
    pub core_mode: Option<String>,
    /// The core's version, once the binary has read it.
    pub version: Option<String>,
    /// Exit IP information for the active route.
    pub ip_info: Option<IpInfo>,
    /// Last lookup error, shown on Home instead of silently dropping it.
    pub ip_error: Option<String>,
    /// When the last successful lookup completed.
    pub ip_updated_at: Option<Instant>,
    /// Whether a lookup is still pending.
    pub ip_refreshing: bool,
    /// Last throughput measurement for the current route.
    pub route_speed: Option<String>,
    /// Last chosen route speed backend and size.
    pub speed_mode: SpeedMode,
    /// The settings, with any unsaved edits.
    pub settings: Settings,
    /// Whether the settings differ from what is on disk.
    pub settings_dirty: bool,
    /// The minimum log level shown and requested from the core.
    pub log_level: LogLevel,
    /// Terminal size, as of the last [`Event::Resize`].
    pub viewport: (u16, u16),
    /// The most recent configuration preview.
    pub preview: Option<Preview>,
    /// How the connection table is ordered.
    pub connection_sort: ConnectionSort,
    /// How members within each proxy group are ordered. Group headings keep
    /// their order from the generated configuration.
    pub node_sort: SortOrder,
    /// Latency method selected for both Proxies and Tests.
    pub probe_mode: ProbeMode,
    /// Whether disabled rules are listed.
    pub show_disabled_rules: bool,
    /// Selected help entry.
    pub help_selected: usize,
    /// First help entry shown.
    pub help_offset: usize,
    last_table_click: Option<(Screen, usize, Instant)>,
    status: Option<Status>,
    last_status: Option<Status>,
    ticks: u64,
    quit: bool,
    chain: Vec<String>,
    all_nodes: Vec<NodeRow>,
    node_delays: HashMap<(ProbeMode, String), Option<u16>>,
    node_health: HashMap<String, (bool, Instant)>,
    expanded: Vec<String>,
    all_rules: Vec<RuleRow>,
    rule_providers: Vec<String>,
    queue: VecDeque<usize>,
    in_flight: Option<usize>,
    frozen: Option<usize>,
    loaded: Vec<Screen>,
}

/// Preserve the tree: only the contiguous children following a group may move.
fn sort_group_members(rows: &mut [NodeRow], order: SortOrder) {
    if !matches!(
        order,
        SortOrder::LatencyAscending | SortOrder::LatencyDescending
    ) {
        return;
    }
    let mut index = 0;
    while index < rows.len() {
        if !rows[index].is_group {
            index += 1;
            continue;
        }
        let group = rows[index].name.clone();
        let start = index + 1;
        let mut end = start;
        while end < rows.len() && rows[end].group.as_deref() == Some(group.as_str()) {
            end += 1;
        }
        rows[start..end].sort_by(|a, b| match order {
            SortOrder::LatencyAscending => a
                .delay
                .unwrap_or(u16::MAX)
                .cmp(&b.delay.unwrap_or(u16::MAX)),
            SortOrder::LatencyDescending => b.delay.unwrap_or(0).cmp(&a.delay.unwrap_or(0)),
            SortOrder::Natural | SortOrder::TrafficDescending => std::cmp::Ordering::Equal,
        });
        index = end;
    }
}

impl App {
    /// Language currently selected for the interface.
    #[must_use]
    pub fn language(&self) -> Language {
        self.settings.ui.language
    }

    /// Translate an interface-owned message, preserving unknown text.
    #[must_use]
    pub fn tr<'a>(&self, english: &'a str) -> &'a str {
        crate::i18n::text(self.language(), english)
    }

    /// Format a status message, translating dynamic patterns in the chosen language.
    #[must_use]
    pub fn format_status_text(&self, text: &str) -> String {
        crate::i18n::format_status(self.language(), text)
    }

    /// Translate a context-specific interface label by its stable identity.
    #[must_use]
    pub(crate) fn tr_key(&self, key: crate::i18n::TextKey) -> &'static str {
        crate::i18n::label(self.language(), key)
    }

    /// A fresh application showing the dashboard, with no data.
    ///
    /// The settings shown are loaded from the home directory, or the defaults
    /// if not present, and updated whenever [`Data::Settings`] arrives.
    #[must_use]
    pub fn new(home: PathBuf, theme: Theme) -> Self {
        let settings =
            cvt_core::Settings::load(&cvt_core::AppPaths::new(&home)).unwrap_or_default();
        Self::with_settings(home, theme, settings)
    }

    /// Construct interaction state from already-loaded settings, without I/O.
    #[must_use]
    pub fn with_settings(home: PathBuf, theme: Theme, settings: Settings) -> Self {
        let log_level = settings.ui.log_level;
        let settings_rows = Table::from_items(setting_rows(&settings));
        let mut app = Self {
            home,
            theme,
            screen: Screen::Home,
            keymap: Keymap::new(),
            overlay: None,
            pending_authorization: None,
            pending_profile_source: None,
            app_update: None,
            app_update_next_check: Instant::now() + Duration::from_secs(3600),
            profiles: Table::new(),
            nodes: Table::new(),
            connections: Table::new(),
            rules: Table::new(),
            tests: Table::new(),
            settings_rows,
            logs: LogBuffer::new(LOG_CAPACITY),
            metrics: Metrics::new(METRIC_SAMPLES),
            core: CoreStatus::Stopped,
            core_mode: None,
            version: None,
            ip_info: None,
            ip_error: None,
            ip_updated_at: None,
            ip_refreshing: false,
            route_speed: None,
            speed_mode: SpeedMode::Sample4,
            settings,
            settings_dirty: false,
            log_level,
            viewport: (80, 24),
            preview: None,
            connection_sort: ConnectionSort::Natural,
            node_sort: SortOrder::Natural,
            probe_mode: ProbeMode::Connect,
            show_disabled_rules: true,
            help_selected: 0,
            help_offset: 0,
            last_table_click: None,
            status: None,
            last_status: None,
            ticks: 0,
            quit: false,
            chain: Vec::new(),
            all_nodes: Vec::new(),
            node_delays: HashMap::new(),
            node_health: HashMap::new(),
            expanded: Vec::new(),
            all_rules: Vec::new(),
            rule_providers: Vec::new(),
            queue: VecDeque::new(),
            in_flight: None,
            frozen: None,
            loaded: Vec::new(),
        };
        app.rebuild_tests();
        if app.settings.core.secret.as_deref() == Some("") {
            app.set_status(
                StatusKind::Warning,
                app.tr("controller secret is empty: API access is unauthenticated")
                    .to_owned(),
            );
        }
        app
    }

    /// A summary of group selections; rules may choose among these per connection.
    #[must_use]
    pub fn selected_proxy_summary(&self) -> String {
        let selected: Vec<_> = self
            .all_nodes
            .iter()
            .filter(|row| row.active && row.group.is_some())
            .take(2)
            .map(|row| format!("{}: {}", row.group.as_deref().unwrap_or_default(), row.name))
            .collect();
        if selected.is_empty() {
            "-".to_owned()
        } else {
            selected.join(" · ")
        }
    }

    fn clear_ip(&mut self) {
        self.ip_info = None;
        self.ip_error = None;
        self.ip_updated_at = None;
        self.ip_refreshing = false;
    }

    // -- observable state ---------------------------------------------------

    /// Whether the application should stop.
    #[must_use]
    pub fn is_quit(&self) -> bool {
        self.quit
    }

    /// The status message to show, if it has not expired.
    #[must_use]
    pub fn current_status(&self) -> Option<&Status> {
        self.status
            .as_ref()
            .filter(|s| !s.is_expired_at(Instant::now()))
    }

    /// The most recent status message shown, even if expired.
    #[must_use]
    pub fn last_status(&self) -> Option<&Status> {
        self.last_status.as_ref()
    }

    /// The explicit patch chain, in application order.
    #[must_use]
    pub fn chain(&self) -> &[String] {
        &self.chain
    }

    /// Whether a group is expanded on the proxies screen.
    #[must_use]
    pub fn is_expanded(&self, group: &str) -> bool {
        self.expanded.iter().any(|g| g == group)
    }

    /// The names of the rule providers the core reported.
    #[must_use]
    pub fn rule_providers(&self) -> &[String] {
        &self.rule_providers
    }

    /// How many tests are queued or in flight.
    ///
    /// A test that has been asked for counts until it answers, so that
    /// cancelling works while the first one of a batch is still running.
    #[must_use]
    pub fn queued_tests(&self) -> usize {
        self.queue.len() + usize::from(self.in_flight.is_some())
    }

    /// How many rows the current screen can show at once.
    #[must_use]
    pub fn visible_rows(&self) -> usize {
        crate::ui::table_rows_area(self).map_or(1, |area| usize::from(area.height).max(1))
    }

    /// How many rules are hidden because they are disabled.
    #[must_use]
    pub fn hidden_rules(&self) -> usize {
        self.all_rules.len().saturating_sub(self.rules.total())
    }

    /// The filter in force on the screen being shown, if any.
    #[must_use]
    pub fn active_filter(&self) -> String {
        self.active_rows().map_or_else(String::new, Rows::filter)
    }

    /// The log lines the screen should show, and how many newer ones it hides.
    ///
    /// Stopping the follow does not discard anything: the window simply stops
    /// moving, and the count of what has arrived since is what the title
    /// reports, so the user can see that the stream is alive without it
    /// dragging the view away from the line being read.
    #[must_use]
    pub fn log_window(&self, height: usize) -> (Vec<&LogRow>, usize) {
        let lines = self.logs.filtered();
        let end = self
            .frozen
            .map_or(lines.len(), |frozen| frozen.min(lines.len()));
        let start = end.saturating_sub(height);
        (lines[start..end].to_vec(), lines.len() - end)
    }

    /// The path the logs would be exported to.
    #[must_use]
    pub fn log_export_path(&self) -> PathBuf {
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        self.home.join("logs").join(format!("cvt-logs-{stamp}.log"))
    }

    /// The generated runtime configuration's path.
    ///
    /// Mirrors `cvt_core::AppPaths::runtime_config`; the home is the one thing
    /// the interface is told about, and a path is all this effect needs.
    #[must_use]
    pub fn runtime_config_path(&self) -> PathBuf {
        self.home.join("runtime").join("config.yaml")
    }

    // -- the entry point ----------------------------------------------------

    /// Handle one event, and return what the binary should do about it.
    pub fn on_event(&mut self, event: Event) -> Vec<Effect> {
        match event {
            Event::Key(key) => self.on_key(key),
            Event::Mouse(mouse) => self.on_mouse(mouse),
            Event::Tick => self.on_tick(),
            Event::Resize(width, height) => {
                self.viewport = (width, height);
                self.sync_scroll();
                Vec::new()
            }
            Event::Data(data) => self.on_data(data),
            Event::Done(done) => self.on_done(done),
            Event::Failed(message) => {
                // A failure can be the answer to anything, including a queued
                // test, so nothing may stay marked as running.
                self.abandon_tests();
                self.set_status(StatusKind::Error, message);
                Vec::new()
            }
        }
    }

    /// Handle a frame tick.
    ///
    /// Expires transient messages and, while the core is up, asks for the
    /// current screen to be re-read now and then — the streams cover the live
    /// panes, but nothing pushes a changed rule list or a new process id.
    pub fn on_tick(&mut self) -> Vec<Effect> {
        self.ticks = self.ticks.wrapping_add(1);
        self.expire_status();
        self.show_pending_app_update();
        if Instant::now() >= self.app_update_next_check && self.app_update.is_none() {
            self.app_update_next_check = Instant::now() + Duration::from_secs(3600);
            return vec![Effect::CheckAppUpdate { manual: false }];
        }
        let before = self.node_health.len();
        self.node_health
            .retain(|_, (_, at)| at.elapsed() < NODE_HEALTH_TTL);
        if self.node_health.len() != before {
            self.rebuild_nodes();
        }
        if self.core.is_running() && self.ticks.is_multiple_of(POLL_EVERY) {
            return Self::refresh_effects(self.screen);
        }
        Vec::new()
    }

    // -- small helpers ------------------------------------------------------

    /// Refuse something the user asked for, with the reason.
    fn refuse(&mut self, reason: impl Into<String>) {
        self.set_status(StatusKind::Warning, reason);
    }

    /// Whether the core is up, refusing the request when it is not.
    fn require_core(&mut self, what: &str) -> bool {
        if self.core.is_running() {
            return true;
        }
        self.refuse(format!(
            "{what} needs a running core; press `s` on Home to start it"
        ));
        false
    }

    fn set_status(&mut self, kind: StatusKind, text: impl Into<String>) {
        let text = text.into();
        let status = Status::new(kind, text);
        self.last_status = Some(status.clone());
        self.status = Some(status);
    }

    /// Open a modal overlay showing the full text of the latest status message.
    pub fn show_last_message(&mut self) -> Vec<Effect> {
        if let Some(status) = self.last_status() {
            let formatted = self.format_status_text(&status.text);
            self.overlay = Some(Overlay::Message {
                title: self.tr("message").to_owned(),
                text: formatted,
                kind: status.kind,
                scroll: 0,
            });
        } else {
            self.overlay = Some(Overlay::Message {
                title: self.tr("message").to_owned(),
                text: self.tr("no message to show").to_owned(),
                kind: StatusKind::Info,
                scroll: 0,
            });
        }
        Vec::new()
    }

    fn expire_status(&mut self) {
        self.expire_status_at(Instant::now());
    }

    /// Drop a transient message once it is older than [`STATUS_TTL`].
    ///
    /// Takes the clock as a parameter so the rule can be tested without
    /// sleeping for four seconds.
    fn expire_status_at(&mut self, now: Instant) {
        if self.status.as_ref().is_some_and(|s| s.is_expired_at(now)) {
            self.status = None;
        }
    }

    fn reload_mode(&self) -> ReloadMode {
        if self.settings.update.prefer_hot_reload {
            ReloadMode::Auto
        } else {
            ReloadMode::Restart
        }
    }
}

/// How many log lines are kept.
const LOG_CAPACITY: usize = 5_000;

/// How many traffic and memory samples the dashboard gauges hold.
const METRIC_SAMPLES: usize = 120;

/// The byte offset of character `index`, or the end of the string.
///
/// Prompts count the caret in characters because that is what the user sees;
/// `String::insert` needs a byte offset, and a multi-byte character in a
/// subscription URL must not split.
fn char_to_byte(text: &str, index: usize) -> usize {
    text.char_indices()
        .nth(index)
        .map_or(text.len(), |(offset, _)| offset)
}

/// A name for a profile created from a URL.
fn name_from_url(url: &str) -> String {
    let trimmed = url
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let host = trimmed.split('/').next().unwrap_or_default();
    let host = host.rsplit('@').next().unwrap_or(host);
    if host.is_empty() {
        "subscription".to_owned()
    } else {
        host.to_owned()
    }
}
