//! Application and core settings.
//!
//! The rows are typed: a switch flips, a number steps through values the
//! validator accepts, a choice cycles, and free text opens a prompt that
//! refuses an invalid answer while it is still open. Nothing here can produce
//! a settings file that would fail to load.

use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::widgets::{Cell, Row};

use crate::app::{App, SettingKind};
use crate::ui::widgets as w;

/// Draw the settings screen.
pub fn render(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let (list_area, detail_area) = w::list_and_detail(area, 5);
    let rows: Vec<Row<'static>> = w::visible(&app.settings_rows)
        .into_iter()
        .map(|setting| {
            let theme = app.theme;
            let value_style = match setting.kind {
                SettingKind::Bool => theme.emphasis(),
                SettingKind::Number { .. } => theme.info(),
                SettingKind::Choice { .. } => theme.warn(),
                SettingKind::Text => theme.key_label(),
            };
            Row::new(vec![
                Cell::from(setting.label).style(theme.key_label()),
                Cell::from(setting.value.clone()).style(value_style),
                Cell::from(setting.help).style(theme.dim()),
            ])
        })
        .collect();
    let dirty = if app.settings_dirty {
        app.tr(" · unsaved changes")
    } else {
        ""
    };
    let title = format!(
        " {} ({}){dirty} ",
        app.tr("settings"),
        app.settings_rows.len()
    );
    w::list(
        frame,
        list_area,
        app,
        w::ListSpec {
            state: w::state_of(&app.settings_rows),
            title,
            header: vec!["setting", "value", "what it does"],
            widths: vec![
                Constraint::Min(26),
                Constraint::Min(16),
                Constraint::Min(28),
            ],
            rows,
            empty: "no setting matches the filter".to_owned(),
        },
    );
    detail(frame, detail_area, app);
}

/// How to change the highlighted row, and where the file lives.
fn detail(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let path = app.home.join("cvt.yaml");
    let how = match app.settings_rows.selected_item().map(|row| row.kind) {
        Some(SettingKind::Bool) => "Enter flips this switch",
        Some(SettingKind::Number { .. } | SettingKind::Choice { .. }) => {
            "Enter or Space cycles this value"
        }
        Some(SettingKind::Text) => "Enter opens a prompt for this value",
        None => "press a to add a profile from the Profiles screen",
    };
    let rows = [
        ("changing a row", app.tr(how).to_owned()),
        (
            "saving",
            if app.settings_dirty {
                app.tr("the file is out of date — press s to write it")
                    .to_owned()
            } else {
                app.tr("everything here is on disk").to_owned()
            },
        ),
        ("file", path.display().to_string()),
        (
            "validation",
            app.tr("an invalid value is refused before it can be written")
                .to_owned(),
        ),
    ];
    w::details(frame, area, app, " settings file ", &rows);
}
