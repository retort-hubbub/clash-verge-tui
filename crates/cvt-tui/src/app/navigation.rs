//! Navigation responsibilities of the application state machine.

use crate::action::Screen;
use crate::state::{Filterable, LogBuffer, Table};

use super::{App, Effect, Overlay, PromptKind, SpeedMode, StatusKind, set_setting_choice};
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// connection ordering
// ---------------------------------------------------------------------------

/// How the connection table is ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConnectionSort {
    /// As the core reported them.
    #[default]
    Natural,
    /// Largest total traffic first.
    Busiest,
    /// Open longest first.
    Oldest,
    /// Most recently opened first.
    Newest,
    /// Largest current upload plus download rate first.
    Fastest,
}

impl ConnectionSort {
    /// The next order in the cycle.
    #[must_use]
    pub fn next(self) -> Self {
        match self {
            Self::Natural => Self::Busiest,
            Self::Busiest => Self::Oldest,
            Self::Oldest => Self::Newest,
            Self::Newest => Self::Fastest,
            Self::Fastest => Self::Natural,
        }
    }

    /// A short label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Natural => "natural",
            Self::Busiest => "total traffic",
            Self::Oldest => "oldest",
            Self::Newest => "newest",
            Self::Fastest => "transfer rate",
        }
    }
}

// ---------------------------------------------------------------------------
// list access
// ---------------------------------------------------------------------------

/// The operations the dispatcher performs on whichever list is on screen.
///
/// Implemented for [`Table`] and for [`LogBuffer`]; the logs screen has a
/// filter but no cursor, so its cursor operations are no-ops. Keeping this as
/// one trait is what lets navigation, paging and search share a single code
/// path instead of a screen-shaped `match` in every handler.
pub(super) trait Rows {
    /// Replace the filter, keeping the cursor on the same row where possible.
    fn set_filter(&mut self, needle: &str);
    /// The current filter.
    fn filter(&self) -> String;
    /// Discard the filter.
    fn clear_filter(&mut self) {
        self.set_filter("");
    }
    /// Move the cursor.
    fn move_by(&mut self, _delta: isize) {}
    /// Move the cursor by a screenful.
    fn page(&mut self, _direction: isize, _viewport: usize) {}
    /// Move the cursor to the first row.
    fn select_first(&mut self) {}
    /// Move the cursor to the last row.
    fn select_last(&mut self) {}
    /// Move the cursor to the next row matching the filter.
    fn next_match(&mut self) {}
    /// Scroll so the cursor is inside a viewport.
    fn scroll_into_view(&mut self, _height: usize) {}
    /// How many rows pass the filter.
    fn len(&self) -> usize;
    /// How many rows there are in total.
    fn total(&self) -> usize;
}

impl<T: Filterable> Rows for Table<T> {
    fn set_filter(&mut self, needle: &str) {
        Self::set_filter(self, needle);
    }

    fn filter(&self) -> String {
        Self::filter(self).to_owned()
    }

    fn move_by(&mut self, delta: isize) {
        Self::move_by(self, delta);
    }

    fn page(&mut self, direction: isize, viewport: usize) {
        Self::page(self, direction, viewport);
    }

    fn select_first(&mut self) {
        Self::select_first(self);
    }

    fn select_last(&mut self) {
        Self::select_last(self);
    }

    fn next_match(&mut self) {
        let needle = Self::filter(self).to_owned();
        if needle.is_empty() {
            Self::move_by(self, 1);
            return;
        }
        self.select_next_matching(|row| row.matches_filter(&needle));
    }

    fn scroll_into_view(&mut self, height: usize) {
        Self::scroll_into_view(self, height);
    }

    fn len(&self) -> usize {
        Self::len(self)
    }

    fn total(&self) -> usize {
        Self::total(self)
    }
}

impl Rows for LogBuffer {
    fn set_filter(&mut self, needle: &str) {
        Self::set_filter(self, needle);
    }

    fn filter(&self) -> String {
        Self::filter(self).to_owned()
    }

    fn len(&self) -> usize {
        self.filtered().len()
    }

    fn total(&self) -> usize {
        Self::len(self)
    }
}

impl App {
    // -- screens and lists --------------------------------------------------

    pub(super) fn goto(&mut self, screen: Screen) -> Vec<Effect> {
        self.screen = screen;
        if screen == Screen::Tests {
            self.rebuild_tests();
        }
        self.sync_scroll();
        if self.loaded.contains(&screen) && screen != Screen::Home {
            return Vec::new();
        }
        self.remember_loaded(screen);
        Self::refresh_effects(screen)
    }

    pub(super) fn remember_loaded(&mut self, screen: Screen) {
        if !self.loaded.contains(&screen) {
            self.loaded.push(screen);
        }
    }

    pub(super) fn refresh_effects(screen: Screen) -> Vec<Effect> {
        match screen {
            // The profile screen's data is the store, which has its own effect.
            Screen::Profiles => vec![Effect::LoadProfiles],
            // The key reference is generated from the key map; nothing to read.
            Screen::Help => Vec::new(),
            other => vec![Effect::Refresh(other)],
        }
    }

    pub(super) fn active_rows(&self) -> Option<&dyn Rows> {
        match self.screen {
            Screen::Profiles => Some(&self.profiles),
            Screen::Proxies => Some(&self.nodes),
            Screen::Connections => Some(&self.connections),
            Screen::Rules => Some(&self.rules),
            Screen::Tests => Some(&self.tests),
            Screen::Settings => Some(&self.settings_rows),
            Screen::Logs => Some(&self.logs),
            Screen::Home | Screen::Help => None,
        }
    }

    pub(super) fn active_rows_mut(&mut self) -> Option<&mut dyn Rows> {
        match self.screen {
            Screen::Profiles => Some(&mut self.profiles),
            Screen::Proxies => Some(&mut self.nodes),
            Screen::Connections => Some(&mut self.connections),
            Screen::Rules => Some(&mut self.rules),
            Screen::Tests => Some(&mut self.tests),
            Screen::Settings => Some(&mut self.settings_rows),
            Screen::Logs => Some(&mut self.logs),
            Screen::Home | Screen::Help => None,
        }
    }

    pub(super) fn active_table_offset(&self) -> usize {
        match self.screen {
            Screen::Profiles => self.profiles.offset(),
            Screen::Proxies => self.nodes.offset(),
            Screen::Connections => self.connections.offset(),
            Screen::Rules => self.rules.offset(),
            Screen::Tests => self.tests.offset(),
            Screen::Settings => self.settings_rows.offset(),
            Screen::Home | Screen::Logs | Screen::Help => 0,
        }
    }

    pub(super) fn select_table_row(&mut self, index: usize) {
        match self.screen {
            Screen::Profiles => self.profiles.select(index),
            Screen::Proxies => self.nodes.select(index),
            Screen::Connections => self.connections.select(index),
            Screen::Rules => self.rules.select(index),
            Screen::Tests => self.tests.select(index),
            Screen::Settings => self.settings_rows.select(index),
            Screen::Home | Screen::Logs | Screen::Help => return,
        }
        self.sync_scroll();
    }

    pub(super) fn scroll_logs(&mut self, delta: isize) {
        let len = self.logs.filtered().len();
        let end = self.frozen.unwrap_or(len).min(len);
        let next = end.saturating_add_signed(delta).min(len);
        self.logs.follow = next == len;
        self.frozen = if self.logs.follow { None } else { Some(next) };
    }

    pub(super) fn move_help(&mut self, delta: isize) {
        let count = crate::ui::help::entries(self).len();
        self.help_selected = self
            .help_selected
            .saturating_add_signed(delta)
            .min(count.saturating_sub(1));
        self.sync_help_scroll();
    }

    pub(super) fn sync_help_scroll(&mut self) {
        let height = self.visible_rows().max(1);
        let max_offset = crate::ui::help::entries(self).len().saturating_sub(height);
        self.help_offset = self.help_offset.min(max_offset);
        if self.help_selected < self.help_offset {
            self.help_offset = self.help_selected;
        } else if self.help_selected >= self.help_offset.saturating_add(height) {
            self.help_offset = self
                .help_selected
                .saturating_sub(height - 1)
                .min(max_offset);
        }
    }

    pub(super) fn sync_scroll(&mut self) {
        if self.screen == Screen::Help {
            self.sync_help_scroll();
            return;
        }
        let height = self.visible_rows();
        if let Some(rows) = self.active_rows_mut() {
            rows.scroll_into_view(height);
        }
    }

    pub(super) fn move_cursor(&mut self, delta: isize) -> Vec<Effect> {
        if self.screen == Screen::Help {
            self.move_help(delta);
            return Vec::new();
        }
        if self.screen == Screen::Logs {
            self.scroll_logs(delta);
            return Vec::new();
        }
        if let Some(rows) = self.active_rows_mut() {
            rows.move_by(delta);
        }
        self.sync_scroll();
        Vec::new()
    }

    pub(super) fn page_cursor(&mut self, direction: isize) -> Vec<Effect> {
        if self.screen == Screen::Help {
            self.move_help(
                direction
                    .saturating_mul(isize::try_from(self.visible_rows()).unwrap_or(isize::MAX)),
            );
            return Vec::new();
        }
        if self.screen == Screen::Logs {
            self.scroll_logs(
                direction
                    .saturating_mul(isize::try_from(self.visible_rows()).unwrap_or(isize::MAX)),
            );
            return Vec::new();
        }
        let height = self.visible_rows();
        if let Some(rows) = self.active_rows_mut() {
            rows.page(direction, height);
        }
        self.sync_scroll();
        Vec::new()
    }

    pub(super) fn move_to_edge(&mut self, first: bool) -> Vec<Effect> {
        if self.screen == Screen::Help {
            let count = crate::ui::help::entries(self).len();
            self.help_selected = if first { 0 } else { count.saturating_sub(1) };
            self.sync_help_scroll();
            return Vec::new();
        }
        if self.screen == Screen::Logs {
            let end = if first { 1 } else { self.logs.filtered().len() };
            self.logs.follow = !first;
            self.frozen = if first { Some(end) } else { None };
            return Vec::new();
        }
        if let Some(rows) = self.active_rows_mut() {
            if first {
                rows.select_first();
            } else {
                rows.select_last();
            }
        }
        self.sync_scroll();
        Vec::new()
    }

    pub(super) fn set_filter(&mut self, needle: &str) {
        if let Some(rows) = self.active_rows_mut() {
            rows.set_filter(needle);
        }
        if self.screen == Screen::Logs {
            self.frozen = if self.logs.follow {
                None
            } else {
                Some(self.logs.filtered().len())
            };
        }
        self.sync_scroll();
    }

    pub(super) fn clear_filter(&mut self) -> bool {
        let had = self.active_rows().is_some_and(|r| !r.filter().is_empty());
        if self.screen == Screen::Logs {
            self.set_filter("");
        } else if let Some(rows) = self.active_rows_mut() {
            rows.clear_filter();
        }
        self.sync_scroll();
        had
    }

    /// `Esc` with nothing else to close: clear the filter, or say so.
    pub(super) fn cancel_scope(&mut self) {
        if self.clear_filter() {
            self.set_status(StatusKind::Info, "filter cleared");
        } else {
            // Esc dismisses the footer immediately; the full text remains
            // available with `m` through `last_status`.
            self.status = None;
        }
    }

    pub(super) fn open_prompt(&mut self, kind: PromptKind, value: String) {
        let cursor = value.chars().count();
        let label = self.prompt_label(kind);
        self.overlay = Some(Overlay::Prompt {
            label,
            kind,
            value,
            cursor,
        });
    }

    /// Run a picker choice.
    pub(super) fn choose(&mut self, title: &str, items: &[String], selected: usize) -> Vec<Effect> {
        if title == super::application_update::UPDATE_TITLE {
            return self.choose_app_update(selected);
        }
        let Some(item) = items.get(selected).cloned() else {
            self.set_status(StatusKind::Warning, "nothing to choose");
            return Vec::new();
        };
        match title {
            "new profile" => {
                if selected == 0 {
                    self.open_prompt(PromptKind::Url, String::new());
                } else {
                    self.open_prompt(PromptKind::Name, String::new());
                }
                Vec::new()
            }
            "import from" => vec![Effect::ImportProfiles {
                source: PathBuf::from(item),
            }],
            "update rule set" => vec![Effect::UpdateRuleProviders { names: vec![item] }],
            "route bandwidth" => {
                let Some(mode) = SpeedMode::from_index(selected) else {
                    return Vec::new();
                };
                self.speed_mode = mode;
                self.route_speed = None;
                self.set_status(
                    StatusKind::Info,
                    format!("measuring current route with {}…", mode.label()),
                );
                vec![Effect::TestRouteSpeed { mode }]
            }
            key if key.starts_with("setting:") => {
                let key = &key[8..];
                if let Err(error) = set_setting_choice(&mut self.settings, key, &item) {
                    self.refuse(error);
                    return Vec::new();
                }
                self.settings_dirty = true;
                let effects = self.after_settings_change();
                self.set_status(StatusKind::Info, format!("{key} = {item}"));
                effects
            }
            other => {
                self.set_status(
                    StatusKind::Info,
                    format!("nothing to do with `{other}` = `{item}`"),
                );
                Vec::new()
            }
        }
    }
}
