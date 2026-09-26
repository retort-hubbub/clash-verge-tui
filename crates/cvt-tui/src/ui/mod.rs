//! The rendering layer.
//!
//! [`render`] is the only entry point the binary needs: it draws the tab bar
//! (with the core's state, which every screen depends on), the active screen,
//! the footer of key hints, and any overlay on top.
//!
//! Every screen is a module exporting the same function —
//! `pub fn render(frame: &mut Frame<'_>, area: Rect, app: &App)` — so the
//! dispatcher below is a lookup and not a special case per screen. Screens
//! never mutate [`App`]: anything that changes state goes through a key press,
//! and a screen that cannot be drawn has nothing to say about it.
//!
//! All of this has to survive a terminal of one column by one row, which is
//! what happens during a resize and inside a tmux pane being dragged. Ratatui
//! clamps zero-sized areas, so the rule the screens follow is: never index a
//! slice with a number the area did not already justify.

pub mod connections;
pub mod help;
pub mod home;
pub mod logs;
pub mod profiles;
pub mod proxies;
pub mod rules;
pub mod settings;
pub mod tests;
pub mod widgets;

#[cfg(test)]
mod render_tests;

use cvt_core::mihomo::supervisor::CoreStatus;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Clear, List, ListState, Paragraph, Tabs, Wrap};

use crate::action::Screen;
use crate::app::{App, Overlay, PromptKind};
use crate::theme::Theme;

use widgets as w;

/// Draw the whole interface.
pub fn render(frame: &mut Frame<'_>, app: &App) {
    let area = frame.area();
    let chrome = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .split(area);
    tab_bar(frame, chrome[0], app);
    screen(frame, chrome[1], app);
    footer(frame, chrome[2], app);
    if let Some(overlay) = &app.overlay {
        draw_overlay(frame, area, app, overlay);
    }
}

/// Draw the screen that is on show.
fn screen(frame: &mut Frame<'_>, area: Rect, app: &App) {
    match app.screen {
        Screen::Home => home::render(frame, area, app),
        Screen::Profiles => profiles::render(frame, area, app),
        Screen::Proxies => proxies::render(frame, area, app),
        Screen::Connections => connections::render(frame, area, app),
        Screen::Logs => logs::render(frame, area, app),
        Screen::Rules => rules::render(frame, area, app),
        Screen::Tests => tests::render(frame, area, app),
        Screen::Settings => settings::render(frame, area, app),
        Screen::Help => help::render(frame, area, app),
    }
}

/// The tab bar, with the core's state and the live rate on the right.
fn tab_bar(frame: &mut Frame<'_>, area: Rect, app: &App) {
    if area.is_empty() {
        return;
    }
    let titles: Vec<Line<'static>> = Screen::all()
        .iter()
        .map(|screen| Line::from(format!(" {} ", screen.title())))
        .collect();
    let index = Screen::all()
        .iter()
        .position(|screen| *screen == app.screen)
        .unwrap_or(0);
    let summary = w::core_summary(app);
    let summary_width = u16::try_from(summary.chars().count()).unwrap_or(u16::MAX);
    // The core state matters more than the tab names, but a tab bar with no
    // tabs in it is not a tab bar: two thirds is the most the summary may take.
    let capped = summary_width.min(area.width.saturating_mul(2) / 3);
    let chunks = Layout::horizontal([Constraint::Min(0), Constraint::Length(capped)]).split(area);
    frame.render_widget(
        Tabs::new(titles)
            .select(index)
            .divider(" ")
            .style(app.theme.key_label())
            .highlight_style(app.theme.tab_active()),
        chunks[0],
    );
    frame.render_widget(
        Paragraph::new(Line::from(summary))
            .style(core_style(&app.core, app.theme))
            .alignment(Alignment::Right),
        chunks[1],
    );
}

/// How the core's state reads.
fn core_style(status: &CoreStatus, theme: Theme) -> ratatui::style::Style {
    match status {
        CoreStatus::Running { .. } => theme.ok(),
        CoreStatus::Stopped => theme.dim(),
        CoreStatus::NotInstalled | CoreStatus::StalePid { .. } => theme.warn(),
    }
}

/// The footer: the status message if there is one, otherwise the key hints.
///
/// A status message is the answer to something the user just did, so it takes
/// the one line available; it expires on its own and the hints come back.
fn footer(frame: &mut Frame<'_>, area: Rect, app: &App) {
    if area.is_empty() {
        return;
    }
    let line = if app.current_status().is_some() {
        w::status_line(app)
    } else if app.settings.ui.show_footer {
        w::hints(app, area.width)
    } else {
        Line::default()
    };
    frame.render_widget(Paragraph::new(line), area);
}

/// Draw the modal layer on top of everything else.
fn draw_overlay(frame: &mut Frame<'_>, area: Rect, app: &App, overlay: &Overlay) {
    match overlay {
        Overlay::Prompt {
            label,
            kind,
            value,
            cursor,
        } => prompt(frame, area, app, label, *kind, value, *cursor),
        Overlay::Confirm { question, action } => confirm(frame, area, app, question, action),
        Overlay::Picker {
            title,
            items,
            selected,
        } => picker(frame, area, app, title, items, *selected),
        Overlay::Preview {
            title,
            lines,
            scroll,
        } => preview(frame, area, app, title, lines, *scroll),
    }
}

/// A one-line input.
fn prompt(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    label: &str,
    kind: PromptKind,
    value: &str,
    cursor: usize,
) {
    let width = area.width.saturating_sub(4).clamp(8, 72);
    let popup = w::centered(area, width, 3);
    frame.render_widget(Clear, popup);

    // The caret is drawn as a selected cell rather than as a character, so a
    // space in the middle of an answer is still visible.
    let chars: Vec<char> = value.chars().collect();
    let cursor = cursor.min(chars.len());
    let before: String = chars[..cursor].iter().collect();
    let after: String = chars[cursor.min(chars.len())..].iter().skip(1).collect();
    let under = chars.get(cursor).copied().unwrap_or(' ');
    let line = Line::from(vec![
        Span::styled(format!(" {before}"), app.theme.key_label()),
        Span::styled(under.to_string(), app.theme.selection()),
        Span::styled(after, app.theme.key_label()),
    ]);
    let hint = match kind {
        PromptKind::Search => "type to narrow the list · Enter keep · Esc clear",
        _ => "Enter accept · Esc cancel",
    };
    let block = w::panel(Line::from(format!(" {label} ")), app.theme)
        .title_bottom(Line::from(format!(" {hint} ")).style(app.theme.key_label()));
    frame.render_widget(Paragraph::new(line).block(block), popup);
}

/// A question about something destructive.
fn confirm(frame: &mut Frame<'_>, area: Rect, app: &App, question: &str, action: &crate::Action) {
    let width = area.width.saturating_sub(4).clamp(12, 72);
    let popup = w::centered(area, width, 5);
    frame.render_widget(Clear, popup);
    let body = Text::from(vec![
        Line::from(Span::styled(question.to_owned(), app.theme.key_label())),
        Line::default(),
        Line::from(vec![
            Span::styled(" [y] yes ", app.theme.error()),
            Span::styled("   ", app.theme.key_label()),
            Span::styled("[n] no ", app.theme.key_label()),
        ]),
    ]);
    let block = w::empty_panel(app.theme)
        .border_style(app.theme.error())
        .title(Line::from(format!(" {} ", action.label())))
        .title_style(app.theme.error());
    frame.render_widget(
        Paragraph::new(body).wrap(Wrap { trim: true }).block(block),
        popup,
    );
}

/// A list of choices.
fn picker(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    title: &str,
    items: &[String],
    selected: usize,
) {
    let width = area.width.saturating_sub(4).clamp(12, 72);
    let height = u16::try_from(items.len())
        .unwrap_or(u16::MAX)
        .saturating_add(2)
        .min(area.height);
    let popup = w::centered(area, width, height);
    frame.render_widget(Clear, popup);
    let rows: Vec<Line<'static>> = items.iter().map(|item| Line::from(item.clone())).collect();
    let list = List::new(rows)
        .block(w::panel(Line::from(format!(" {title} ")), app.theme))
        .highlight_style(app.theme.selection())
        .highlight_symbol("▸ ");
    let mut state =
        ListState::default().with_selected(Some(selected.min(items.len().saturating_sub(1))));
    frame.render_stateful_widget(list, popup, &mut state);
}

/// Scrolling text: the configuration preview.
fn preview(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    title: &str,
    lines: &[String],
    scroll: usize,
) {
    let popup = w::centered(
        area,
        area.width.saturating_sub(4),
        area.height.saturating_sub(2),
    );
    frame.render_widget(Clear, popup);
    let text = Text::from(
        lines
            .iter()
            .map(|line| Line::from(line.clone()))
            .collect::<Vec<_>>(),
    );
    let block = w::panel(Line::from(format!(" {title} ")), app.theme).title_bottom(
        Line::from(" j/k scroll · PgUp/PgDn page · Esc close ").style(app.theme.key_label()),
    );
    let offset = u16::try_from(scroll).unwrap_or(u16::MAX);
    frame.render_widget(
        Paragraph::new(text)
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((offset, 0)),
        popup,
    );
}
