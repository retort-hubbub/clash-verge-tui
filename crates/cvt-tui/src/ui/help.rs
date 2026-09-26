//! The key reference.
//!
//! Nothing here is written by hand. The rows come from the key map, grouped by
//! `Action::group`, labelled by `Action::label`, described by `Action::help`
//! and keyed by `Keymap::keys_for`, so a binding that changes in the key map
//! changes here in the same commit. A help screen that can drift from the key
//! map is worse than no help screen at all.
//!
//! The reference does not fit on one screen and the key map binds no movement
//! here, so the layout adapts instead: on a wide terminal the sections are
//! spread over two columns, which brings almost the whole map into view at
//! once. A `List` would need a cursor the user cannot move.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::{Cell, Row, TableState};

use crate::app::App;
use crate::ui::widgets as w;

/// Width at which two columns of the reference fit side by side.
const TWO_COLUMNS_FROM: u16 = 110;

/// Draw the key reference.
pub fn render(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let sections = sections(app);
    if sections.is_empty() {
        w::message(frame, area, app.theme, "the key map is empty");
        return;
    }
    let columns = if area.width >= TWO_COLUMNS_FROM { 2 } else { 1 };
    let areas = Layout::horizontal(std::iter::repeat_n(Constraint::Fill(1), columns)).split(area);

    for (index, rows) in distribute(sections, columns).into_iter().enumerate() {
        let (Some(column), false) = (areas.get(index), rows.is_empty()) else {
            continue;
        };
        w::list(
            frame,
            *column,
            app,
            w::ListSpec {
                // The reference has no cursor: the key map binds no movement
                // on this screen, so a highlight would be a lie.
                state: TableState::default(),
                title: if index == 0 {
                    format!(" key reference ({columns} column(s)) ")
                } else {
                    String::new()
                },
                header: vec!["keys", "applies", "action", "what it does"],
                widths: vec![
                    Constraint::Length(11),
                    Constraint::Length(13),
                    Constraint::Min(16),
                    Constraint::Min(24),
                ],
                rows,
                empty: String::new(),
            },
        );
    }
}

/// One section per action group, in the order the key map mentions them.
fn sections(app: &App) -> Vec<(String, Vec<Row<'static>>)> {
    let mut sections: Vec<(String, Vec<Row<'static>>)> = Vec::new();
    let mut seen: Vec<(&'static str, &'static str)> = Vec::new();
    for binding in app.keymap.bindings() {
        let action = &binding.action;
        let group = action.group();
        let label = action.label();
        // An action reachable by several keys, or from several screens, is
        // listed once: the keys column already names every binding.
        if seen.contains(&(group, label)) {
            continue;
        }
        seen.push((group, label));
        let keys = app.keymap.keys_for(action);
        let row = Row::new(vec![
            Cell::from(if keys.is_empty() {
                "-".to_owned()
            } else {
                keys.join(" / ")
            })
            .style(app.theme.key_hint()),
            Cell::from(binding.context.label()).style(app.theme.dim()),
            Cell::from(label).style(app.theme.key_label()),
            Cell::from(action.help()).style(app.theme.dim()),
        ]);
        match sections.last_mut() {
            Some((name, rows)) if name == group => rows.push(row),
            _ => sections.push((group.to_owned(), vec![heading(group, app), row])),
        }
    }
    sections
}

/// A group heading, drawn as a row of its own.
fn heading(group: &str, app: &App) -> Row<'static> {
    Row::new(vec![
        Cell::from(group.to_owned()).style(app.theme.emphasis()),
    ])
}

/// Spread sections over `columns` columns, balancing by height.
///
/// A section is never split across columns: a group heading without its rows
/// reads as an error.
fn distribute(
    sections: Vec<(String, Vec<Row<'static>>)>,
    columns: usize,
) -> Vec<Vec<Row<'static>>> {
    let mut out: Vec<Vec<Row<'static>>> = vec![Vec::new(); columns];
    let total: usize = sections.iter().map(|(_, rows)| rows.len()).sum();
    let target = total / columns.max(1);
    let mut index = 0;
    let mut filled = 0;
    for (_, rows) in sections {
        let length = rows.len();
        if index + 1 < columns && filled >= target {
            index += 1;
            filled = 0;
        }
        filled += length;
        out[index].extend(rows);
    }
    out
}
