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
    let (list_area, detail_area) = super::list_and_detail_for(area, crate::Screen::Proxies);
    let rows: Vec<Row<'static>> = w::visible(&app.nodes)
        .into_iter()
        .map(|node| row(node, app))
        .collect();
    let groups = app.nodes.items().iter().filter(|row| row.is_group).count();
    let title = crate::i18n::message(
        app.language(),
        crate::i18n::Message::ProxiesTitle {
            groups,
            shown: app.nodes.len(),
            total: app.nodes.total(),
        },
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
                Constraint::Length(10),
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
    // Some terminals count emoji variation selectors as an extra cell while
    // Ratatui counts the grapheme as one. Strip the selector for display only:
    // the original name remains the key used for selection and API calls.
    let display_name = node.name.replace('\u{fe0f}', "");
    let name = if node.is_group {
        Cell::from(format!(
            "{} {}",
            if app.is_expanded(&node.name) {
                "▾"
            } else {
                "▸"
            },
            display_name
        ))
        .style(name_style)
    } else {
        Cell::from(format!("   {display_name}")).style(name_style)
    };
    let group = node.group.clone().unwrap_or_else(|| "-".to_owned());
    let kind = if node.is_group {
        app.tr(node.group_kind_label()).to_owned()
    } else {
        node.kind.clone()
    };
    let state = if !app.core.is_running() {
        Cell::from(app.tr("offline")).style(theme.dim())
    } else if node.active {
        Cell::from(app.tr("active")).style(theme.ok())
    } else if !node.alive {
        Cell::from(app.tr_key(crate::i18n::TextKey::ProxyUnavailable)).style(theme.error())
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
    rows.push(("test mode", app.probe_mode.label().to_owned()));
    rows.push(("bandwidth mode", app.tr(app.speed_mode.label()).to_owned()));
    rows.push((
        "route speed (b)",
        app.route_speed
            .clone()
            .unwrap_or_else(|| app.tr("not tested").to_owned()),
    ));
    rows.push((
        "method",
        app.tr(if app.probe_mode == crate::row::ProbeMode::Connect {
            "CONNECT uses the named proxy; v cycles test methods"
        } else {
            "TCP and ICMP probe the server directly; v cycles test methods"
        })
        .to_owned(),
    ));
    match app.nodes.selected_item() {
        Some(node) if node.is_group => {
            rows.push(("group", node.name.clone()));
            rows.push(("behaviour", app.tr(node.group_kind_label()).to_owned()));
            rows.push(("members", node.members.to_string()));
            rows.push((
                "pinning",
                if !app.core.is_running() {
                    app.tr("start the core to choose a member").to_owned()
                } else if node.selectable {
                    app.tr("Enter on a member pins it; x clears the choice")
                        .to_owned()
                } else {
                    app.tr("the core picks the member itself; it cannot be pinned")
                        .to_owned()
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
                app.tr(if !app.core.is_running() {
                    "offline"
                } else if node.alive {
                    "alive"
                } else {
                    "not answering"
                })
                .to_owned(),
            ));
            rows.push((
                "hint",
                app.tr("Enter pins this node in its group, x lets the group choose again")
                    .to_owned(),
            ));
        }
        None => rows.push((
            "hint",
            app.tr(
                "apply a profile or start the core; t tests, T tests the group, a tests everything",
            )
            .to_owned(),
        )),
    }
    rows.push(("order", app.tr(app.node_sort.label()).to_owned()));
    w::details(frame, area, app, " selection ", &rows);
}
