//! Streaming and AI availability checks, with queued batch progress.

use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::widgets::{Cell, Row};

use crate::app::App;
use crate::row::{TestResult, TestRow};
use crate::ui::widgets as w;

/// Draw the tests screen.
pub fn render(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let (list_area, detail_area) = super::list_and_detail_for(area, crate::Screen::Tests);
    let rows: Vec<Row<'static>> = w::visible(&app.tests)
        .into_iter()
        .map(|check| row(check, app))
        .collect();
    let queued = app.queued_tests();
    let title = crate::i18n::message(app.language(), crate::i18n::Message::TestsTitle(queued));
    w::list(
        frame,
        list_area,
        app,
        w::ListSpec {
            state: w::state_of(&app.tests),
            title,
            header: vec!["check", "route", "result"],
            widths: vec![
                Constraint::Length(20),
                Constraint::Min(20),
                Constraint::Min(14),
            ],
            rows,
            empty: "no unlock checks are available".to_owned(),
        },
    );
    detail(frame, detail_area, app);
}

fn row(check: &TestRow, app: &App) -> Row<'static> {
    let theme = app.theme;
    let (value, style) = match &check.result {
        TestResult::Pending => (app.tr("pending").to_owned(), theme.dim()),
        TestResult::Running => (app.tr("running…").to_owned(), theme.warn()),
        TestResult::Passed(value) => (value.clone(), theme.ok()),
        TestResult::Failed(reason) => (reason.clone(), theme.error()),
    };
    Row::new(vec![
        Cell::from(app.tr(check.kind.label()).to_owned()).style(theme.key_label()),
        Cell::from(check.target.clone()).style(theme.dim()),
        Cell::from(value).style(style),
    ])
}

/// What the highlighted check does, and how to run it.
fn detail(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let mut rows: Vec<(&str, String)> = Vec::new();
    match app.tests.selected_item() {
        Some(test) => {
            rows.push(("check", app.tr(test.kind.label()).to_owned()));
            rows.push(("target", test.target.clone()));
            rows.push(("what it does", app.tr(test.kind.description()).to_owned()));
            let result = match &test.result {
                TestResult::Failed(reason)
                    if matches!(test.kind, crate::row::TestKind::Unlock(_)) =>
                {
                    reason.clone()
                }
                other => other.label(),
            };
            rows.push(("result", result));
        }
        None => rows.push((
            "hint",
            app.tr("Enter runs the highlighted check").to_owned(),
        )),
    }
    w::details(frame, area, app, " selected check ", &rows);
}
