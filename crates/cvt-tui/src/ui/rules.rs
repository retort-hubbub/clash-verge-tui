//! Routing rules and rule sets.
//!
//! Two things matter on this screen: whether a rule is even enabled, and
//! whether it has ever matched. A rule that has been evaluated thousands of
//! times and never fired is almost always shadowed by an earlier one, which is
//! invisible in every other tool, so it is called out here.

use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::widgets::{Cell, Row};

use crate::app::App;
use crate::row::RuleRow;
use crate::ui::widgets as w;

/// How many evaluations make a rule look dead rather than merely new.
const DEAD_AFTER: u64 = 1_000;

/// Draw the rules screen.
pub fn render(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let (list_area, detail_area) = w::list_and_detail(area, 7);
    let rows: Vec<Row<'static>> = w::visible(&app.rules)
        .into_iter()
        .map(|rule| row(rule, app))
        .collect();
    let hidden = app.hidden_rules();
    let title = crate::i18n::message(
        app.language(),
        crate::i18n::Message::RulesTitle {
            shown: app.rules.len(),
            hidden,
        },
    );
    w::list(
        frame,
        list_area,
        app,
        w::ListSpec {
            state: w::state_of(&app.rules),
            title,
            header: vec!["#", "type", "value", "policy", "hits"],
            widths: vec![
                Constraint::Length(6),
                Constraint::Length(18),
                Constraint::Min(18),
                Constraint::Length(14),
                Constraint::Length(10),
            ],
            rows,
            empty: "no rules — apply a profile, or press h to include the disabled ones".to_owned(),
        },
    );
    detail(frame, detail_area, app);
}

fn row(rule: &RuleRow, app: &App) -> Row<'static> {
    let theme = app.theme;
    let style = if rule.disabled {
        theme.disabled()
    } else {
        theme.key_label()
    };
    let hits = if rule.looks_dead(DEAD_AFTER) {
        Cell::from(rule.hits_label()).style(theme.warn())
    } else {
        Cell::from(rule.hits_label()).style(theme.dim())
    };
    Row::new(vec![
        Cell::from(rule.index.to_string()).style(theme.dim()),
        Cell::from(rule.kind.clone()).style(theme.emphasis()),
        Cell::from(rule.payload.clone()).style(style),
        Cell::from(rule.policy.clone()).style(style),
        hits,
    ])
    .style(style)
}

/// The selected rule as it would be written, plus its counters.
fn detail(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let mut rows: Vec<(&str, String)> = Vec::new();
    match app.rules.selected_item() {
        Some(rule) => {
            rows.push(("rule", rule.raw.clone()));
            rows.push((
                "state",
                app.tr(if rule.disabled { "disabled" } else { "enabled" })
                    .to_owned(),
            ));
            rows.push((
                "counters",
                if rule.has_stats() {
                    crate::i18n::message(
                        app.language(),
                        crate::i18n::Message::RuleCounters {
                            hits: rule.hits,
                            misses: rule.misses,
                        },
                    )
                } else {
                    app.tr("the core reports no counters for this rule")
                        .to_owned()
                },
            ));
            if rule.looks_dead(DEAD_AFTER) {
                rows.push((
                    "note",
                    crate::i18n::message(
                        app.language(),
                        crate::i18n::Message::RuleNeverMatched(rule.misses),
                    ),
                ));
            }
        }
        None => rows.push((
            "hint",
            app.tr("Enter toggles the highlighted rule; h includes disabled rules")
                .to_owned(),
        )),
    }
    let providers = app.rule_providers();
    rows.push((
        "rule sets",
        if providers.is_empty() {
            app.tr("none reported by the core").to_owned()
        } else {
            format!("{}: {}", providers.len(), providers.join(", "))
        },
    ));
    w::details(frame, area, app, " selected rule ", &rows);
}
