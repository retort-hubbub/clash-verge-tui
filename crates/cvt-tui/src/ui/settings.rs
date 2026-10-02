//! Application and core settings.
//!
//! Switches toggle directly, choices open a picker, and text prompts validate input.

use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::widgets::{Cell, Row};

use crate::app::{App, SettingKind};
use crate::ui::widgets as w;

/// Draw the settings screen.
pub fn render(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let (list_area, detail_area) = super::list_and_detail_for(area, crate::Screen::Settings);
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
            header: vec!["setting", "value"],
            widths: vec![Constraint::Percentage(48), Constraint::Percentage(52)],
            rows,
            empty: "no setting matches the filter".to_owned(),
        },
    );
    detail(frame, detail_area, app);
}

/// How to change the highlighted row, and where the file lives.
fn detail(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let path = app.home.join("cvt.yaml");
    let rows = [
        (
            "what it does",
            app.settings_rows
                .selected_item()
                .map_or_else(String::new, |row| app.tr(row.help).to_owned()),
        ),
        ("file", path.display().to_string()),
    ];
    w::details(frame, area, app, " settings file ", &rows);
}
