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
    let visible = w::visible(&app.connections);
    let (columns, widths) = columns(list_area.width, app);
    let rows = visible
        .into_iter()
        .map(|connection| row(connection, app, &columns))
        .collect();
    let title = crate::i18n::message(
        app.language(),
        crate::i18n::Message::ConnectionsTitle {
            count: app.connections.len(),
            sort: app.tr(app.connection_sort.label()),
        },
    );
    w::list_with_spacing(
        frame,
        list_area,
        app,
        w::ListSpec {
            state: w::state_of(&app.connections),
            title,
            header: columns.iter().map(|&index| HEADERS[index]).collect(),
            widths,
            rows,
            empty: "no connections — the core reports them only while it is running".to_owned(),
        },
        COLUMN_SPACING,
    );
    detail(frame, detail_area, app);
}

const HEADERS: [&str; 8] = [
    "destination",
    "net",
    "process",
    "rule",
    "chain",
    "transfer rate",
    "total traffic",
    "since",
];

const COLUMN_SPACING: u16 = 3;

fn values(connection: &ConnectionRow, app: &App) -> [String; 8] {
    [
        connection.destination.clone(),
        connection.network.clone(),
        connection.process.clone(),
        connection.rule.clone(),
        connection.chain.clone(),
        app.connection_rate_label(&connection.id),
        connection.traffic_label(),
        connection.started.clone(),
    ]
}

/// Keep the layout independent of connection snapshots so live updates cannot
/// move columns or change which metadata is visible.
fn columns(width: u16, app: &App) -> (Vec<usize>, Vec<Constraint>) {
    use unicode_width::UnicodeWidthStr as _;
    let preferred = [0, 6, 16, 18, 20, 28, 24, 10];
    let sizes = std::array::from_fn::<_, 8, _>(|i| {
        preferred[i].max(u16::try_from(app.tr(HEADERS[i]).width()).unwrap_or(u16::MAX))
    });
    let available = width.saturating_sub(2);
    // A fifth of the viewport, with a 16-cell minimum and no upper cap.
    let destination_width = (available / 5).max(16);
    let mut selected = vec![0, 5, 6];
    let mut used = destination_width
        .saturating_add(sizes[5])
        .saturating_add(sizes[6])
        .saturating_add(2 * COLUMN_SPACING);
    for index in [1, 2, 4, 3, 7] {
        if used
            .saturating_add(sizes[index])
            .saturating_add(COLUMN_SPACING)
            <= available
        {
            selected.push(index);
            used += sizes[index] + COLUMN_SPACING;
        }
    }
    selected.sort_unstable();
    let widths = selected
        .iter()
        .map(|&index| {
            if index == 0 {
                Constraint::Length(destination_width)
            } else {
                Constraint::Length(sizes[index])
            }
        })
        .collect();
    (selected, widths)
}

fn row(connection: &ConnectionRow, app: &App, columns: &[usize]) -> Row<'static> {
    let values = values(connection, app);
    Row::new(columns.iter().map(|&index| {
        let style = match index {
            5 => app.theme.traffic(),
            3 | 6 => app.theme.info(),
            1 | 4 | 7 => app.theme.dim(),
            _ => app.theme.key_label(),
        };
        Cell::from(values[index].clone()).style(style)
    }))
}

/// Traffic and metadata for the selected connection.
fn detail(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let mut rows: Vec<(&str, String)> = Vec::new();
    match app.connections.selected_item() {
        Some(connection) => {
            rows.push(("transfer rate", app.connection_rate_label(&connection.id)));
            rows.push(("total traffic", connection.traffic_label()));
            rows.push(("destination", connection.destination.clone()));
            rows.push(("process", connection.process.clone()));
            rows.push(("rule", connection.rule.clone()));
            rows.push(("chain", connection.chain.clone()));
        }
        None => rows.push((
            "hint",
            app.tr("d closes the highlighted connection; s changes the sort order")
                .to_owned(),
        )),
    }
    w::details(frame, area, app, " selected connection ", &rows);
}
