//! Input responsibilities of the application state machine.

use crate::action::{Action, Screen};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use super::{
    App, DOUBLE_CLICK_WINDOW, Effect, Overlay, PromptKind, Rows, StatusKind, char_to_byte,
    name_from_url, set_setting_text,
};
use std::time::Instant;

impl App {
    // -- key routing --------------------------------------------------------

    pub(super) fn on_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        // An overlay is modal: while one is open the key map is not consulted
        // at all, which is what stops `q` from quitting out of a prompt.
        if self.overlay.is_some() {
            return self.on_overlay_key(key);
        }
        if self.screen == Screen::Help && key.code == KeyCode::Enter {
            self.show_help_entry();
            return Vec::new();
        }
        match self.keymap.resolve(self.screen, key) {
            Some(action) => self.dispatch(action, false),
            None => Vec::new(),
        }
    }

    pub(super) fn on_mouse(&mut self, mouse: MouseEvent) -> Vec<Effect> {
        if self.overlay.is_some() {
            return self.on_overlay_mouse(mouse);
        }
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if mouse.row == self.viewport.1.saturating_sub(1) {
                    self.last_table_click = None;
                    if self.current_status().is_some() {
                        return self.show_last_message();
                    }
                    if let Some(action) = crate::ui::footer_action_at(self, mouse.column) {
                        return self.dispatch(action, false);
                    }
                    return Vec::new();
                }
                if let Some(screen) = crate::ui::tab_at(self, mouse.column, mouse.row) {
                    self.last_table_click = None;
                    return self.goto(screen);
                }
                if mouse.row == 0 {
                    self.last_table_click = None;
                    self.inspect_core_summary();
                    return Vec::new();
                }
                if self.screen == Screen::Home {
                    self.last_table_click = None;
                    self.inspect_home();
                    return Vec::new();
                }
                if self.screen == Screen::Logs {
                    self.last_table_click = None;
                    self.inspect_log_at(mouse.column, mouse.row);
                    return Vec::new();
                }
                if crate::ui::detail_area(self).is_some_and(|area| {
                    area.contains(ratatui::layout::Position::new(mouse.column, mouse.row))
                }) {
                    self.last_table_click = None;
                    self.inspect_selection();
                    return Vec::new();
                }
                self.click_table_row(mouse.column, mouse.row, false)
            }
            MouseEventKind::Down(MouseButton::Right) => {
                self.last_table_click = None;
                if mouse.row == self.viewport.1.saturating_sub(1) {
                    return self.show_last_message();
                }
                if mouse.row == 0 {
                    self.inspect_core_summary();
                    return Vec::new();
                }
                if self.screen == Screen::Logs {
                    self.inspect_log_at(mouse.column, mouse.row);
                    return Vec::new();
                }
                if self.screen == Screen::Home {
                    self.inspect_home();
                    return Vec::new();
                }
                if !self
                    .click_table_row(mouse.column, mouse.row, true)
                    .is_empty()
                {
                    return Vec::new();
                }
                if self.overlay.is_none() {
                    self.inspect_selection();
                }
                Vec::new()
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                if mouse.row > 0 && mouse.row < self.viewport.1.saturating_sub(1) =>
            {
                self.last_table_click = None;
                let delta = if matches!(mouse.kind, MouseEventKind::ScrollUp) {
                    -3
                } else {
                    3
                };
                if self.screen == Screen::Logs {
                    self.scroll_logs(delta);
                    Vec::new()
                } else {
                    self.move_cursor(delta)
                }
            }
            _ => Vec::new(),
        }
    }

    pub(super) fn click_table_row(&mut self, column: u16, row: u16, inspect: bool) -> Vec<Effect> {
        if self.screen == Screen::Help {
            self.last_table_click = None;
            if let Some(index) = crate::ui::help::entry_at(self, column, row) {
                self.help_selected = index;
                if inspect {
                    self.show_help_entry();
                }
            }
            return Vec::new();
        }
        let Some(area) = crate::ui::table_rows_area(self) else {
            self.last_table_click = None;
            return Vec::new();
        };
        if !area.contains(ratatui::layout::Position::new(column, row)) {
            self.last_table_click = None;
            return Vec::new();
        }
        let offset = self.active_table_offset();
        let index = offset + usize::from(row.saturating_sub(area.y));
        if self.active_rows().is_none_or(|rows| index >= rows.len()) {
            self.last_table_click = None;
            return Vec::new();
        }
        let second_click = self.last_table_click.is_some_and(|(screen, previous, at)| {
            screen == self.screen && previous == index && at.elapsed() <= DOUBLE_CLICK_WINDOW
        });
        self.select_table_row(index);
        if inspect {
            self.last_table_click = None;
            self.inspect_selection();
            return Vec::new();
        }
        if self.screen == Screen::Proxies
            && column == area.x
            && self.nodes.selected_item().is_some_and(|node| node.is_group)
        {
            self.last_table_click = None;
            return self.dispatch(Action::SelectNode, false);
        }
        self.last_table_click = Some((self.screen, index, Instant::now()));
        if second_click {
            self.last_table_click = None;
            let action = match self.screen {
                Screen::Profiles => Action::ActivateProfile,
                Screen::Proxies => Action::SelectNode,
                Screen::Connections => Action::CloseConnection,
                Screen::Rules => Action::ToggleRule,
                Screen::Tests => Action::RunTests,
                Screen::Settings => Action::ToggleSetting,
                Screen::Home | Screen::Logs | Screen::Help => return Vec::new(),
            };
            return self.dispatch(action, false);
        }
        Vec::new()
    }

    pub(super) fn on_overlay_mouse(&mut self, mouse: MouseEvent) -> Vec<Effect> {
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            let Some(overlay) = self.overlay.clone() else {
                return Vec::new();
            };
            match overlay {
                Overlay::Prompt { .. } => {
                    if let Some(accept) = crate::ui::prompt_choice_at(self, mouse.column, mouse.row)
                    {
                        let key = if accept { KeyCode::Enter } else { KeyCode::Esc };
                        return self.on_overlay_key(KeyEvent::new(key, KeyModifiers::NONE));
                    }
                }
                Overlay::Confirm { question, .. } => {
                    if let Some(accept) =
                        crate::ui::confirm_choice_at(self, &question, mouse.column, mouse.row)
                    {
                        let key = if accept { KeyCode::Enter } else { KeyCode::Esc };
                        return self.on_overlay_key(KeyEvent::new(key, KeyModifiers::NONE));
                    }
                }
                Overlay::Picker {
                    title,
                    items,
                    selected,
                } => {
                    let hit = if title == super::application_update::UPDATE_TITLE {
                        crate::ui::application_update::choice_at(
                            self.viewport,
                            mouse.column,
                            mouse.row,
                        )
                    } else {
                        crate::ui::picker_item_at(
                            self.viewport,
                            items.len(),
                            selected,
                            mouse.column,
                            mouse.row,
                        )
                    };
                    if let Some(index) = hit {
                        self.overlay = None;
                        return self.choose(&title, &items, index);
                    }
                }
                Overlay::Preview { .. } | Overlay::Message { .. } => {
                    self.overlay = None;
                }
            }
            return Vec::new();
        }
        let Some(overlay) = self.overlay.as_mut() else {
            return Vec::new();
        };
        match (overlay, mouse.kind) {
            (
                Overlay::Message { scroll, .. } | Overlay::Preview { scroll, .. },
                MouseEventKind::ScrollUp,
            ) => {
                *scroll = scroll.saturating_sub(3);
            }
            (Overlay::Message { text, scroll, .. }, MouseEventKind::ScrollDown) => {
                let last = crate::ui::message_scroll_limit(self.viewport, text);
                *scroll = scroll.saturating_add(3).min(last);
            }
            (Overlay::Picker { selected, .. }, MouseEventKind::ScrollUp) => {
                *selected = selected.saturating_sub(3);
            }
            (
                Overlay::Picker {
                    items, selected, ..
                },
                MouseEventKind::ScrollDown,
            ) => {
                *selected = selected
                    .saturating_add(3)
                    .min(items.len().saturating_sub(1));
            }
            (Overlay::Preview { lines, scroll, .. }, MouseEventKind::ScrollDown) => {
                *scroll = scroll
                    .saturating_add(3)
                    .min(crate::ui::preview_scroll_limit(self.viewport, lines));
            }
            _ => {}
        }
        Vec::new()
    }

    pub(super) fn on_overlay_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(overlay) = self.overlay.take() else {
            return Vec::new();
        };
        match overlay {
            Overlay::Prompt {
                kind,
                value,
                cursor,
                ..
            } => self.on_prompt_key(kind, value, cursor, key),
            Overlay::Confirm { question, action } => {
                if matches!(key.code, KeyCode::Char('y' | 'Y') | KeyCode::Enter) {
                    return self.dispatch(action, true);
                }
                if matches!(key.code, KeyCode::Char('n' | 'N') | KeyCode::Esc) {
                    self.set_status(StatusKind::Info, "cancelled");
                    return Vec::new();
                }
                // Anything else is swallowed rather than treated as a no.
                self.overlay = Some(Overlay::Confirm { question, action });
                Vec::new()
            }
            Overlay::Picker {
                title,
                items,
                selected,
            } => {
                let last = items.len().saturating_sub(1);
                match key.code {
                    KeyCode::Esc => {
                        if title == super::application_update::UPDATE_TITLE {
                            return self.choose_app_update(1);
                        }
                        self.set_status(StatusKind::Info, "cancelled");
                    }
                    KeyCode::Enter => return self.choose(&title, &items, selected),
                    KeyCode::Down | KeyCode::Char('j') => {
                        self.overlay = Some(Overlay::Picker {
                            title,
                            items,
                            selected: selected.saturating_add(1).min(last),
                        });
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        self.overlay = Some(Overlay::Picker {
                            title,
                            items,
                            selected: selected.saturating_sub(1),
                        });
                    }
                    KeyCode::Home | KeyCode::Char('g') => {
                        self.overlay = Some(Overlay::Picker {
                            title,
                            items,
                            selected: 0,
                        });
                    }
                    KeyCode::End | KeyCode::Char('G') => {
                        self.overlay = Some(Overlay::Picker {
                            title,
                            items,
                            selected: last,
                        });
                    }
                    _ => {
                        self.overlay = Some(Overlay::Picker {
                            title,
                            items,
                            selected,
                        });
                    }
                }
                Vec::new()
            }
            Overlay::Preview {
                title,
                lines,
                scroll,
            } => {
                let last = crate::ui::preview_scroll_limit(self.viewport, &lines);
                let page = crate::ui::preview_page_rows(self.viewport, &lines);
                let next = match key.code {
                    KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => None,
                    KeyCode::Down | KeyCode::Char('j') => Some(scroll.saturating_add(1).min(last)),
                    KeyCode::Up | KeyCode::Char('k') => Some(scroll.saturating_sub(1)),
                    KeyCode::PageDown | KeyCode::Char(' ') => {
                        Some(scroll.saturating_add(page).min(last))
                    }
                    KeyCode::PageUp => Some(scroll.saturating_sub(page)),
                    KeyCode::Home | KeyCode::Char('g') => Some(0),
                    KeyCode::End | KeyCode::Char('G') => Some(last),
                    _ => Some(scroll),
                };
                match next {
                    Some(scroll) => {
                        self.overlay = Some(Overlay::Preview {
                            title,
                            lines,
                            scroll,
                        });
                    }
                    None => self.set_status(StatusKind::Info, "preview closed"),
                }
                Vec::new()
            }
            Overlay::Message {
                title,
                text,
                kind,
                scroll,
            } => {
                let limit = crate::ui::message_scroll_limit(self.viewport, &text);
                let next = match key.code {
                    KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => return Vec::new(),
                    KeyCode::Down | KeyCode::Char('j') => scroll.saturating_add(1).min(limit),
                    KeyCode::Up | KeyCode::Char('k') => scroll.saturating_sub(1),
                    KeyCode::PageDown => scroll.saturating_add(self.visible_rows()).min(limit),
                    KeyCode::PageUp => scroll.saturating_sub(self.visible_rows()),
                    KeyCode::Home => 0,
                    KeyCode::End => limit,
                    _ => scroll,
                };
                self.overlay = Some(Overlay::Message {
                    title,
                    text,
                    kind,
                    scroll: next,
                });
                Vec::new()
            }
        }
    }

    pub(super) fn on_prompt_key(
        &mut self,
        kind: PromptKind,
        mut value: String,
        mut cursor: usize,
        key: KeyEvent,
    ) -> Vec<Effect> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                if kind == PromptKind::Search {
                    self.set_filter("");
                }
                self.set_status(StatusKind::Info, "cancelled");
                return Vec::new();
            }
            KeyCode::Enter => return self.commit_prompt(kind, &value),
            KeyCode::Backspace => {
                if cursor > 0 {
                    let from = char_to_byte(&value, cursor - 1);
                    let to = char_to_byte(&value, cursor);
                    value.replace_range(from..to, "");
                    cursor -= 1;
                }
            }
            KeyCode::Delete => {
                if cursor < value.chars().count() {
                    let from = char_to_byte(&value, cursor);
                    let to = char_to_byte(&value, cursor + 1);
                    value.replace_range(from..to, "");
                }
            }
            KeyCode::Left => cursor = cursor.saturating_sub(1),
            KeyCode::Right => {
                cursor = cursor.saturating_add(1).min(value.chars().count());
            }
            KeyCode::Home => cursor = 0,
            KeyCode::End => cursor = value.chars().count(),
            KeyCode::Char('u') if ctrl => {
                value.clear();
                cursor = 0;
            }
            KeyCode::Char(c) if !ctrl => {
                let at = char_to_byte(&value, cursor);
                value.insert(at, c);
                cursor += 1;
            }
            _ => {}
        }
        if kind == PromptKind::Search {
            self.set_filter(&value);
        }
        self.overlay = Some(Overlay::Prompt {
            label: self.prompt_label(kind),
            kind,
            value,
            cursor,
        });
        Vec::new()
    }

    pub(super) fn prompt_label(&self, kind: PromptKind) -> String {
        if kind != PromptKind::Text {
            return kind.label().to_owned();
        }
        self.settings_rows
            .selected_item()
            .map_or_else(|| kind.label().to_owned(), |row| row.label.to_owned())
    }

    /// Commit a prompt.
    pub(super) fn commit_prompt(&mut self, kind: PromptKind, value: &str) -> Vec<Effect> {
        match kind {
            PromptKind::Search => {
                let matched = self.active_rows().map_or(0, Rows::len);
                let matched_total = self.active_rows().map_or(0, Rows::total);
                if value.is_empty() {
                    self.set_filter("");
                    self.set_status(StatusKind::Info, "filter cleared");
                } else {
                    self.set_status(
                        StatusKind::Info,
                        format!("filtering `{value}` — {matched} of {matched_total} rows"),
                    );
                }
                Vec::new()
            }
            PromptKind::Rename => {
                let Some(uid) = self.profiles.selected_item().map(|row| row.uid.clone()) else {
                    self.refuse("no profile selected");
                    return Vec::new();
                };
                let name = value.trim();
                if name.is_empty() {
                    self.reopen_prompt(kind, value, "a profile needs a name");
                    return Vec::new();
                }
                vec![Effect::RenameProfile {
                    uid,
                    name: name.to_owned(),
                }]
            }
            PromptKind::Url => {
                let url = value.trim();
                if !url.starts_with("http://") && !url.starts_with("https://") {
                    self.reopen_prompt(
                        kind,
                        value,
                        "a subscription URL must start with http:// or https://",
                    );
                    return Vec::new();
                }
                vec![Effect::NewProfile {
                    name: name_from_url(url),
                    url: Some(url.to_owned()),
                }]
            }
            PromptKind::Name => {
                let name = value.trim();
                if name.is_empty() {
                    self.reopen_prompt(kind, value, "a profile needs a name");
                    return Vec::new();
                }
                vec![Effect::NewProfile {
                    name: name.to_owned(),
                    url: None,
                }]
            }
            PromptKind::ProfileUrl => {
                let url = value.trim();
                if !(url.starts_with("https://") || url.starts_with("http://")) {
                    self.reopen_prompt(
                        kind,
                        value,
                        "a subscription URL must start with http:// or https://",
                    );
                    return Vec::new();
                }
                let Some(uid) = self.pending_profile_source.take() else {
                    self.refuse("no profile selected");
                    return Vec::new();
                };
                vec![Effect::EditProfileSource {
                    uid,
                    url: url.to_owned(),
                }]
            }
            PromptKind::Rule => {
                let rule = value.trim();
                if cvt_core::model::rule::Rule::parse(rule).is_none() {
                    self.reopen_prompt(kind, value, "invalid rule; use TYPE,payload,policy");
                    return Vec::new();
                }
                vec![Effect::AddProfileRule {
                    rule: rule.to_owned(),
                }]
            }
            PromptKind::Text => self.commit_text_setting(value),
        }
    }

    /// Refuse what was typed and leave the prompt open with it.
    ///
    /// Closing a prompt on a rejected answer throws the typing away and makes
    /// the user start again; keeping it open means one correction, not one
    /// re-entry.
    pub(super) fn reopen_prompt(&mut self, kind: PromptKind, value: &str, reason: &str) {
        self.set_status(StatusKind::Warning, reason);
        let cursor = value.chars().count();
        let label = self.prompt_label(kind);
        self.overlay = Some(Overlay::Prompt {
            label,
            kind,
            value: value.to_owned(),
            cursor,
        });
    }

    pub(super) fn commit_text_setting(&mut self, value: &str) -> Vec<Effect> {
        let Some(row) = self.settings_rows.selected_item() else {
            self.refuse("no setting selected");
            return Vec::new();
        };
        let key = row.key;
        if let Err(reason) = set_setting_text(&mut self.settings, key, value) {
            self.reopen_prompt(PromptKind::Text, value, &reason);
            return Vec::new();
        }
        self.settings_dirty = true;
        let effects = self.after_settings_change();
        self.set_status(StatusKind::Info, format!("{key} = {}", value.trim()));
        effects
    }
}
