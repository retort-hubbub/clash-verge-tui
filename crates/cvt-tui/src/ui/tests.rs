//! Latency tests.
//!
//! The screen is a queue rather than a set of independent buttons: a batch
//! runs one check at a time, and the title says how many are still waiting, so
//! a user who pressed Enter on four rows knows whether the last one has
//! started yet.

use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::widgets::{Cell, Row};

use crate::app::{App, StatusKind};
use crate::row::{TestResult, TestRow};
use crate::ui::widgets as w;

/// Draw the tests screen.
pub fn render(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let (list_area, detail_area) = w::list_and_detail(area, 9);
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
            header: vec!["check", "target", "result"],
            widths: vec![
                Constraint::Length(14),
                Constraint::Min(20),
                Constraint::Min(14),
            ],
            rows,
            empty: "no checks are available yet — load a profile so there is something to test"
                .to_owned(),
        },
    );
    detail(frame, detail_area, app);
}

fn row(check: &TestRow, app: &App) -> Row<'static> {
    let theme = app.theme;
    let (value, style) = match &check.result {
        TestResult::Pending => (app.tr("pending").to_owned(), theme.dim()),
        TestResult::Running => (app.tr("running…").to_owned(), theme.warn()),
        TestResult::Passed(value) => (format!("{} — {value}", app.tr("ok")), theme.ok()),
        TestResult::Failed(reason) => (format!("{} — {reason}", app.tr("failed")), theme.error()),
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
            rows.push(("result", test.result.label()));
        }
        None => rows.push((
            "hint",
            app.tr("Enter runs the highlighted check").to_owned(),
        )),
    }
    rows.push((
        "batch",
        if app.queued_tests() == 0 {
            app.tr("nothing running").to_owned()
        } else {
            crate::i18n::message(
                app.language(),
                crate::i18n::Message::QueuedTestsHint(app.queued_tests()),
            )
        },
    ));
    if app.core.is_running() {
        rows.push(("core", crate::i18n::core_status(app.language(), &app.core)));
    } else {
        rows.push((
            "core",
            app.tr("not running; latency checks need it, so start it from Home")
                .to_owned(),
        ));
    }
    if let Some(status) = app.current_status()
        && status.kind == StatusKind::Warning
    {
        rows.push(("note", status.text.clone()));
    }
    w::details(frame, area, app, " selected check ", &rows);
}
