//! Subscriptions and the configuration chain.
//!
//! The list is only half the story: what the core will actually run is the
//! base profile plus the patches in the chain, in order. That sequence is
//! shown above the selection so that a patch which is present but excluded is
//! visible before the user wonders why their edit did nothing.

use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::widgets::{Cell, Row};

use crate::app::App;
use crate::row::ProfileRow;
use crate::ui::widgets as w;

/// Draw the profiles screen.
pub fn render(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let (list_area, detail_area) = w::list_and_detail(area, 9);
    let rows: Vec<Row<'static>> = w::visible(&app.profiles)
        .into_iter()
        .map(|profile| row(profile, app))
        .collect();
    let title = crate::i18n::message(
        app.language(),
        crate::i18n::Message::ProfilesTitle {
            shown: app.profiles.len(),
            total: app.profiles.total(),
        },
    );
    w::list(
        frame,
        list_area,
        app,
        w::ListSpec {
            state: w::state_of(&app.profiles),
            title,
            header: vec!["name", "role", "updated", "remaining"],
            widths: vec![
                Constraint::Min(14),
                Constraint::Length(20),
                Constraint::Length(12),
                Constraint::Length(10),
            ],
            rows,
            empty: "no profiles yet — press `a` to add one".to_owned(),
        },
    );
    detail(frame, detail_area, app);
}

fn row(profile: &ProfileRow, app: &App) -> Row<'static> {
    let style = if profile.is_usable() {
        app.theme.key_label()
    } else {
        app.theme.disabled()
    };
    let name_style = if profile.current {
        app.theme.emphasis()
    } else {
        style
    };
    let mut cells = vec![
        Cell::from(profile.name.clone()).style(name_style),
        Cell::from(role_label(profile, app)).style(style),
        Cell::from(profile.updated_label(now())).style(app.theme.dim()),
    ];
    cells.push(match profile.quota_label() {
        Some(quota) => Cell::from(quota).style(app.theme.warn()),
        None => Cell::from("-").style(app.theme.dim()),
    });
    Row::new(cells).style(style)
}

fn role_label(profile: &ProfileRow, app: &App) -> String {
    crate::i18n::profile_role(app.language(), profile)
}

/// What will be generated, and everything odd about the selected profile.
fn detail(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let chain = if app.chain().is_empty() {
        app.tr("base only (no patches are chained)").to_owned()
    } else {
        app.chain().join(" → ")
    };
    let mut rows = vec![("chain", chain)];
    if let Some(profile) = app.profiles.selected_item() {
        rows.push(("profile", profile.name.clone()));
        rows.push(("uid", profile.uid.clone()));
        rows.push((
            "source",
            profile
                .url
                .clone()
                .unwrap_or_else(|| app.tr("local document").to_owned()),
        ));
        rows.push(("updated", profile.updated_label(now())));
        if let Some(reason) = &profile.unsupported {
            rows.push(("cannot run", reason.clone()));
        }
        if let Some(edits) = profile.edits {
            rows.push(("edits", edits.to_string()));
        }
    } else {
        rows.push((
            "hint",
            app.tr("Enter switches profile, c chains a patch, p previews")
                .to_owned(),
        ));
    }
    w::details(frame, area, app, " chain and selection ", &rows);
}

/// Seconds since the epoch, for the update ages.
fn now() -> i64 {
    chrono::Local::now().timestamp()
}
