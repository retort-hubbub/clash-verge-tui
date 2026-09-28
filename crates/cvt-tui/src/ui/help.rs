//! Key reference generated from the active key map.
//!
//! Bindings are grouped as in v0.6.0 and spread over two columns when there
//! is room. The selected explanation occupies a small pane below the groups.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::{Cell, Row, TableState};

use crate::app::App;
use crate::ui::widgets as w;

const TWO_COLUMNS_FROM: u16 = 110;
type HelpRow = (Option<usize>, Row<'static>);

/// A readable binding, including its complete description.
#[derive(Debug, Clone)]
pub struct HelpEntry {
    /// Action group.
    pub group: String,
    /// Every key bound to this action.
    pub keys: String,
    /// Context in which the keys work.
    pub context: String,
    /// Short action name.
    pub action: String,
    /// Complete explanation.
    pub description: String,
}

/// Build one entry per action from the key map.
#[must_use]
pub fn entries(app: &App) -> Vec<HelpEntry> {
    let mut entries = Vec::new();
    let mut seen = Vec::new();
    for binding in app.keymap.bindings() {
        let action = &binding.action;
        let identity = (action.group(), action.label());
        if seen.contains(&identity) {
            continue;
        }
        seen.push(identity);
        let keys = app.keymap.keys_for(action);
        entries.push(HelpEntry {
            group: app.tr(action.group()).to_owned(),
            keys: if keys.is_empty() {
                "-".to_owned()
            } else {
                keys.join(" / ")
            },
            context: app.tr(binding.context.label()).to_owned(),
            action: crate::i18n::action_label(app.language(), action).to_owned(),
            description: app.tr(action.help()).to_owned(),
        });
    }
    entries
}

/// Draw grouped shortcut tables and the selected action below them.
pub fn render(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let entries = entries(app);
    if entries.is_empty() {
        w::message(frame, area, app.theme, app.tr("the key map is empty"));
        return;
    }
    let (list_area, detail_area) = super::list_and_detail_for(area, crate::Screen::Help);
    let columns = columns(&entries, app, list_area.width);
    let areas = Layout::horizontal(std::iter::repeat_n(Constraint::Fill(1), columns.len()))
        .split(list_area);
    for (column_index, (rows, column_area)) in columns.into_iter().zip(areas.iter()).enumerate() {
        let selected = rows
            .iter()
            .position(|(index, _)| *index == Some(app.help_selected));
        let offset = column_offset(&rows, selected, column_area.height);
        w::list(
            frame,
            *column_area,
            app,
            w::ListSpec {
                state: TableState::new()
                    .with_selected(selected)
                    .with_offset(offset),
                title: if column_index == 0 {
                    format!(" {} ", app.tr("key reference"))
                } else {
                    String::new()
                },
                header: vec!["keys", "applies", "action"],
                widths: vec![
                    Constraint::Length(11),
                    Constraint::Length(13),
                    Constraint::Min(16),
                ],
                rows: rows.into_iter().map(|(_, row)| row).collect(),
                empty: String::new(),
            },
        );
    }
    if let Some(entry) = entries.get(app.help_selected) {
        w::details(
            frame,
            detail_area,
            app,
            " selected action ",
            &[
                ("keys", entry.keys.clone()),
                ("what it does", entry.description.clone()),
            ],
        );
    }
}

/// Resolve clicks against the same grouped columns and scroll offset as render.
pub(crate) fn entry_at(app: &App, column: u16, row: u16) -> Option<usize> {
    let content = Rect::new(0, 1, app.viewport.0, app.viewport.1.saturating_sub(2));
    let (list_area, _) = super::list_and_detail_for(content, crate::Screen::Help);
    let entries = entries(app);
    let columns = columns(&entries, app, list_area.width);
    let areas = Layout::horizontal(std::iter::repeat_n(Constraint::Fill(1), columns.len()))
        .split(list_area);
    columns.iter().zip(areas.iter()).find_map(|(rows, area)| {
        let data = Rect::new(
            area.x.saturating_add(1),
            area.y.saturating_add(2),
            area.width.saturating_sub(2),
            area.height.saturating_sub(3),
        );
        if !data.contains(ratatui::layout::Position::new(column, row)) {
            return None;
        }
        let selected = rows
            .iter()
            .position(|(index, _)| *index == Some(app.help_selected));
        let offset = column_offset(rows, selected, area.height);
        rows.get(offset + usize::from(row - data.y))
            .and_then(|(entry, _)| *entry)
    })
}

fn column_offset(rows: &[HelpRow], selected: Option<usize>, height: u16) -> usize {
    let visible = usize::from(height.saturating_sub(3)).max(1);
    selected
        .unwrap_or(0)
        .saturating_sub(visible - 1)
        .min(rows.len().saturating_sub(visible))
}

fn columns(entries: &[HelpEntry], app: &App, width: u16) -> Vec<Vec<HelpRow>> {
    let mut sections: Vec<(String, Vec<HelpRow>)> = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let row = Row::new(vec![
            Cell::from(entry.keys.clone()).style(app.theme.key_hint()),
            Cell::from(entry.context.clone()).style(app.theme.dim()),
            Cell::from(entry.action.clone()).style(app.theme.key_label()),
        ]);
        match sections.last_mut() {
            Some((group, rows)) if group == &entry.group => rows.push((Some(index), row)),
            _ => sections.push((
                entry.group.clone(),
                vec![
                    (
                        None,
                        Row::new(vec![Cell::from(entry.group.clone())]).style(app.theme.emphasis()),
                    ),
                    (Some(index), row),
                ],
            )),
        }
    }
    let count = if width >= TWO_COLUMNS_FROM { 2 } else { 1 };
    let mut out = vec![Vec::new(); count];
    let target = sections.iter().map(|(_, rows)| rows.len()).sum::<usize>() / count;
    let mut column = 0;
    let mut filled = 0;
    for (_, rows) in sections {
        if column + 1 < count && filled >= target {
            column += 1;
            filled = 0;
        }
        filled += rows.len();
        out[column].extend(rows);
    }
    out
}
