//! Live connections.
//!
//! The list is a snapshot the core pushes several times a second, so the
//! renderer must never assume a row it saw last frame still exists. The sort
//! order is in the panel title because a list that silently reorders itself is
//! how a user loses the connection they were about to close.

use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::widgets::{Cell, Row};

use crate::app::App;
use crate::row::ConnectionRow;
use crate::ui::widgets as w;

/// Draw the connections screen.
pub fn render(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let (list_area, detail_area) = super::list_and_detail_for(area, crate::Screen::Connections);
    let rows: Vec<Row<'static>> = w::visible(&app.connections)
        .into_iter()
        .map(|connection| row(connection, app))
        .collect();
    let title = crate::i18n::message(
        app.language(),
        crate::i18n::Message::ConnectionsTitle {
            count: app.connections.len(),
            sort: app.tr(app.connection_sort.label()),
        },
    );
    w::list(
        frame,
        list_area,
        app,
        w::ListSpec {
            state: w::state_of(&app.connections),
            title,
            header: vec![
                "destination",
                "net",
                "process",
                "rule",
                "chain",
                "traffic",
                "since",
            ],
            widths: vec![
                Constraint::Min(18),
                Constraint::Length(4),
                Constraint::Length(12),
                Constraint::Min(12),
                Constraint::Min(10),
                Constraint::Length(16),
                Constraint::Length(8),
            ],
            rows,
            empty: "no connections — the core reports them only while it is running".to_owned(),
        },
    );
    detail(frame, detail_area, app);
}

fn row(connection: &ConnectionRow, app: &App) -> Row<'static> {
    let theme = app.theme;
    Row::new(vec![
        Cell::from(connection.destination.clone()).style(theme.key_label()),
        Cell::from(connection.network.clone()).style(theme.dim()),
        Cell::from(connection.process.clone()).style(theme.key_label()),
        Cell::from(connection.rule.clone()).style(theme.info()),
        Cell::from(connection.chain.clone()).style(theme.dim()),
        Cell::from(connection.traffic_label()).style(theme.traffic()),
        Cell::from(connection.started.clone()).style(theme.dim()),
    ])
}

/// The selected connection in full, and the totals for the table.
fn detail(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let mut rows: Vec<(&str, String)> = Vec::new();
    match app.connections.selected_item() {
        Some(connection) => {
            rows.push(("id", connection.id.clone()));
            rows.push(("destination", connection.destination.clone()));
            rows.push(("process", connection.process.clone()));
            rows.push(("rule", connection.rule.clone()));
            rows.push(("chain", connection.chain.clone()));
            rows.push(("traffic", connection.traffic_label()));
            rows.push(("opened", connection.started.clone()));
        }
        None => rows.push((
            "hint",
            app.tr("d closes the highlighted connection; s changes the sort order")
                .to_owned(),
        )),
    }
    let total: u64 = app
        .connections
        .items()
        .iter()
        .map(ConnectionRow::total)
        .sum();
    rows.push((
        "table",
        crate::i18n::message(
            app.language(),
            crate::i18n::Message::ConnectionTotal {
                shown: app.connections.len(),
                total: app.connections.total(),
                bytes: &crate::state::human_bytes(total),
            },
        ),
    ));
    w::details(frame, area, app, " selected connection ", &rows);
}
