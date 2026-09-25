//! Proxy groups and nodes.
//!
//! The list has two levels: a group row, and the members it can choose
//! between. Groups start collapsed because a subscription routinely holds
//! hundreds of nodes and a wall of them hides the group the user came for;
//! pressing Enter on a group opens it, and pressing Enter on a member pins
//! that member in its group.

use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::widgets::{Cell, Row};

use crate::app::App;
use crate::row::NodeRow;
use crate::ui::widgets as w;

/// Draw the proxies screen.
pub fn render(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let (list_area, detail_area) = w::list_and_detail(area, 8);
    let rows: Vec<Row<'static>> = w::visible(&app.nodes)
        .into_iter()
        .map(|node| row(node, app))
        .collect();
    let groups = app.nodes.items().iter().filter(|row| row.is_group).count();
    let title = format!(
        " proxies ({groups} group(s) · {} of {} rows shown) ",
        app.nodes.len(),
        app.nodes.total()
    );
    w::list(
        frame,
        list_area,
        app,
        w::ListSpec {
            state: w::state_of(&app.nodes),
            title,
            header: vec!["node", "group", "type", "delay", ""],
            widths: vec![
                Constraint::Min(16),
                Constraint::Length(12),
                Constraint::Length(12),
                Constraint::Length(9),
                Constraint::Length(7),
            ],
            rows,
            empty: "no proxies yet — apply a profile, or start the core to see its groups"
                .to_owned(),
        },
    );
    detail(frame, detail_area, app);
}

fn row(node: &NodeRow, app: &App) -> Row<'static> {
    let theme = app.theme;
    let name_style = if node.active {
        theme.emphasis()
    } else {
        theme.key_label()
    };
    let name = if node.is_group {
        Cell::from(format!(
            "{} {}",
            if app.is_expanded(&node.name) {
                "▾"
            } else {
                "▸"
            },
            node.name
        ))
        .style(name_style)
    } else {
        Cell::from(format!("   {}", node.name)).style(name_style)
    };
    let group = node.group.clone().unwrap_or_else(|| "-".to_owned());
    let kind = if node.is_group {
        node.group_kind_label().to_owned()
    } else {
        node.kind.clone()
    };
    let state = if node.active {
        Cell::from("active").style(theme.ok())
    } else if !node.alive {
        Cell::from("down").style(theme.error())
    } else {
        Cell::from("").style(theme.dim())
    };
    Row::new(vec![
        name,
        Cell::from(group).style(theme.dim()),
        Cell::from(kind).style(theme.key_label()),
        Cell::from(w::delay_span(node.delay, theme)),
        state,
    ])
}

/// Everything the selection can do, and why it sometimes cannot.
fn detail(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let mut rows: Vec<(&str, String)> = Vec::new();
    match app.nodes.selected_item() {
        Some(node) if node.is_group => {
            rows.push(("group", node.name.clone()));
            rows.push(("behaviour", node.group_kind_label().to_owned()));
            rows.push(("members", node.members.to_string()));
            rows.push((
                "expanded",
                if app.is_expanded(&node.name) {
                    "yes — Enter collapses it"
                } else {
                    "no — Enter opens it"
                }
                .to_owned(),
            ));
            rows.push((
                "pinning",
                if node.selectable {
                    "Enter on a member pins it; x clears the choice".to_owned()
                } else {
                    "the core picks the member itself; it cannot be pinned".to_owned()
                },
            ));
        }
        Some(node) => {
            rows.push(("node", node.name.clone()));
            rows.push((
                "group",
                node.group.clone().unwrap_or_else(|| "-".to_owned()),
            ));
            rows.push(("type", node.kind.clone()));
            rows.push(("delay", node.delay_label()));
            rows.push((
                "health",
                if node.alive { "alive" } else { "not answering" }.to_owned(),
            ));
            rows.push((
                "hint",
                "Enter pins this node in its group, x lets the group choose again".to_owned(),
            ));
        }
        None => rows.push((
            "hint",
            "apply a profile or start the core; t tests, T tests the group, a tests everything"
                .to_owned(),
        )),
    }
    if app.node_sort != crate::state::SortOrder::Natural {
        rows.push(("order", app.node_sort.label().to_owned()));
    }
    w::details(frame, area, app.theme, " selection ", &rows);
}
