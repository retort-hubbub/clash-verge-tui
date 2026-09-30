//! The rendering layer.
//!
//! [`render`] is the only entry point the binary needs: it draws the tab bar
//! (with the core's state, which every screen depends on), the active screen,
//! the footer of key hints, and any overlay on top.
//!
//! Every screen is a module exporting the same function —
//! `pub fn render(frame: &mut Frame<'_>, area: Rect, app: &App)` — so the
//! dispatcher below is a lookup and not a special case per screen. Screens
//! never mutate [`App`]: anything that changes state goes through an input event,
//! and a screen that cannot be drawn has nothing to say about it.
//!
//! All of this has to survive a terminal of one column by one row, which is
//! what happens during a resize and inside a tmux pane being dragged. Ratatui
//! clamps zero-sized areas, so the rule the screens follow is: never index a
//! slice with a number the area did not already justify.

pub(crate) mod application_update;
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
use unicode_width::UnicodeWidthStr;

use crate::action::Screen;
use crate::app::{App, Overlay};
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
    let index = Screen::all()
        .iter()
        .position(|screen| *screen == app.screen)
        .unwrap_or(0);
    let summary = w::core_summary(app);
    let summary_width = u16::try_from(summary.width()).unwrap_or(u16::MAX);
    // All nine direct keys need to remain visible on a standard 80-column
    // terminal. The dashboard still shows core state below the tabs.
    let capped = summary_cap(area.width, summary_width);
    let chunks = Layout::horizontal([Constraint::Min(0), Constraint::Length(capped)]).split(area);
    let compact = compact_tabs(area.width, chunks[0].width, long_tabs_width(app));
    let titles: Vec<Line<'static>> = Screen::all()
        .iter()
        .enumerate()
        .map(|(at, screen)| {
            let name = tab_name(app, *screen, compact);
            Line::from(vec![
                Span::styled(format!("[{}]", at + 1), app.theme.key_hint()),
                Span::styled(
                    name.to_owned(),
                    if at == index {
                        app.theme.tab_active()
                    } else {
                        app.theme.key_label()
                    },
                ),
            ])
        })
        .collect();
    frame.render_widget(
        Tabs::new(titles)
            .select(index)
            .divider(" ")
            .highlight_style(ratatui::style::Style::default()),
        chunks[0],
    );
    frame.render_widget(
        Paragraph::new(Line::from(summary))
            .style(core_style(&app.core, app.theme))
            .alignment(Alignment::Right),
        chunks[1],
    );
}

fn summary_cap(width: u16, summary_width: u16) -> u16 {
    if width < 100 {
        0
    } else {
        summary_width.min(width / 3)
    }
}

fn compact_tabs(width: u16, tab_width: u16, long_width: usize) -> bool {
    width < 100 || long_width > usize::from(tab_width)
}

fn long_tabs_width(app: &App) -> usize {
    Screen::all()
        .iter()
        .map(|screen| crate::i18n::tab_title(app.language(), *screen).width() + 5)
        .sum::<usize>()
        + 8
}

fn tab_name(app: &App, screen: Screen, compact: bool) -> &'static str {
    if compact {
        crate::i18n::short_tab(app.language(), screen)
    } else {
        crate::i18n::tab_title(app.language(), screen)
    }
}

/// The screen under a click in the tab bar, using the same widths as `Tabs`.
pub(crate) fn tab_at(app: &App, column: u16, row: u16) -> Option<Screen> {
    if row != 0 || column >= app.viewport.0 {
        return None;
    }
    let width = app.viewport.0;
    let summary_width = u16::try_from(w::core_summary(app).width()).unwrap_or(u16::MAX);
    let tab_width = width.saturating_sub(summary_cap(width, summary_width));
    if column >= tab_width {
        return None;
    }
    let compact = compact_tabs(width, tab_width, long_tabs_width(app));
    let mut start = 0usize;
    for (index, screen) in Screen::all().into_iter().enumerate() {
        let name = tab_name(app, screen, compact);
        // One cell of padding on each side; one divider between tabs.
        let end = start + 2 + 3 + name.width() + usize::from(index < 8);
        if usize::from(column) < end {
            return Some(screen);
        }
        start = end;
    }
    None
}

/// The data cells in the current table, excluding its border and header.
pub(crate) fn table_rows_area(app: &App) -> Option<Rect> {
    let detail_height = match app.screen {
        Screen::Help => {
            let content = Rect::new(0, 1, app.viewport.0, app.viewport.1.saturating_sub(2));
            let (list, _) = w::list_and_detail(content, detail_height(Screen::Help));
            return Some(Rect::new(
                list.x.saturating_add(1),
                list.y.saturating_add(2),
                list.width.saturating_sub(2),
                list.height.saturating_sub(3),
            ));
        }
        Screen::Home | Screen::Logs => return None,
        screen => detail_height(screen),
    };
    let content = Rect::new(0, 1, app.viewport.0, app.viewport.1.saturating_sub(2));
    let (list, _) = w::list_and_detail(content, detail_height);
    Some(Rect::new(
        list.x.saturating_add(1),
        list.y.saturating_add(2),
        list.width.saturating_sub(2),
        list.height.saturating_sub(3),
    ))
}

/// Shared pane height for rendering and mouse hit testing.
pub(crate) const fn detail_height(screen: Screen) -> u16 {
    match screen {
        Screen::Profiles => 9,
        Screen::Proxies => 10,
        Screen::Connections | Screen::Rules => 7,
        Screen::Tests | Screen::Settings => 6,
        Screen::Help => 5,
        Screen::Home | Screen::Logs => 0,
    }
}

/// Split a screen using the same dimensions used by mouse hit testing.
pub(crate) fn list_and_detail_for(area: Rect, screen: Screen) -> (Rect, Rect) {
    w::list_and_detail(area, detail_height(screen))
}

/// The visible detail pane, when the terminal has room for one.
pub(crate) fn detail_area(app: &App) -> Option<Rect> {
    if detail_height(app.screen) == 0 {
        return None;
    }
    let content = Rect::new(0, 1, app.viewport.0, app.viewport.1.saturating_sub(2));
    let (_, detail) = list_and_detail_for(content, app.screen);
    (!detail.is_empty()).then_some(detail)
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
        w::status_line(app, area.width)
    } else if app.settings.ui.show_footer {
        w::hints(app, area.width)
    } else {
        Line::default()
    };
    frame.render_widget(Paragraph::new(line), area);
}

/// Resolve a click on the same hint spans that the footer renders.
pub(crate) fn footer_action_at(app: &App, column: u16) -> Option<crate::Action> {
    if app.current_status().is_some() || !app.settings.ui.show_footer {
        return None;
    }
    let limit = usize::from((app.viewport.0 / 16).clamp(1, 8));
    let mut start = 0usize;
    for (key, action) in app.keymap.hint_bindings(app.screen, limit) {
        let label = crate::i18n::action_label(app.language(), action);
        let end = start + format!(" {key} {label} ").width();
        if (start..end).contains(&usize::from(column)) && end <= usize::from(app.viewport.0) {
            return Some(action.clone());
        }
        start = end;
    }
    None
}

/// Draw the modal layer on top of everything else.
fn draw_overlay(frame: &mut Frame<'_>, area: Rect, app: &App, overlay: &Overlay) {
    match overlay {
        Overlay::Prompt {
            label,
            value,
            cursor,
            ..
        } => prompt(frame, area, app, label, value, *cursor),
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
        Overlay::Message {
            title,
            text,
            kind,
            scroll,
        } => {
            message_popup(frame, area, app, title, text, *kind, *scroll);
        }
    }
}

/// A one-line input.
fn prompt(frame: &mut Frame<'_>, area: Rect, app: &App, label: &str, value: &str, cursor: usize) {
    let popup = prompt_popup_rect(area);
    let width = popup.width;
    frame.render_widget(Clear, popup);

    // The caret is drawn as a selected cell rather than as a character, so a
    // space in the middle of an answer is still visible.
    //
    // The window follows the caret. Without that, a value longer than the popup
    // was clipped by the `Paragraph` and the caret — which is at the *end* of
    // anything being typed — went off the edge with it, so the one cell that
    // says where the next character lands was the first thing lost. The leading
    // space is part of the window, so the arithmetic is over `inner + 1` cells.
    let chars: Vec<char> = value.chars().collect();
    let cursor = cursor.min(chars.len());
    // Four cells are not the value's: the two borders, the leading space, and
    // the caret cell itself — which is a space past the end whenever the cursor
    // is there, which is where it is while anything is being typed.
    let inner = usize::from(width).saturating_sub(4);
    // The window is measured in *columns* and walks back from the cursor until
    // the cells are used up, rather than counting characters: one CJK character
    // is two cells, and taking `inner` characters of them overflowed the popup
    // by exactly as many as were wide.
    // One of those cells is the caret's, which is a space past the end of the
    // value whenever the cursor is there — where it is while anything is being
    // typed. Reserving it is what stops a value that exactly fills the popup
    // from pushing the caret off the edge.
    let room = inner.saturating_sub(1);
    let mut start = cursor;
    let mut cells = 0;
    while start > 0 {
        // The true width, not a minimum of one: a combining mark occupies no
        // cell of its own, and counting it as one walked the window back a
        // character too few.
        let wide = cell_width(chars[start - 1]);
        if cells + wide > room {
            break;
        }
        cells += wide;
        start -= 1;
    }
    let visible: String = chars[start..].iter().collect();
    // A **character** index into `visible`, not the column count: `cells` is
    // columns, and using it here took `before` past the cursor by one character
    // for every wide one — 30 columns of CJK is 15 characters, so `take(30)`
    // reached 60 columns and pushed the caret off the popup entirely.
    let caret = cursor - start;
    let mut before: String = visible.chars().take(caret).collect();
    let after: String = visible.chars().skip(caret + 1).collect();
    let mut under = visible.chars().nth(caret).unwrap_or(' ');
    // A combining mark has no cell to put a caret in, and the cell it *would*
    // take is the one its base character is already using. Moving it behind the
    // caret keeps it attached to its base — where the text puts it — and gives
    // the caret a cell of its own, instead of the zero-width cell it cannot be
    // drawn in.
    if under != ' ' && cell_width(under) == 0 {
        before.push(under);
        under = ' ';
    }
    let line = Line::from(vec![
        Span::styled(format!(" {before}"), app.theme.key_label()),
        Span::styled(under.to_string(), app.theme.selection()),
        Span::styled(after, app.theme.key_label()),
    ]);
    let buttons = Line::from(vec![
        Span::styled(app.tr(" [Enter] accept "), app.theme.ok()),
        Span::styled("   ", app.theme.key_label()),
        Span::styled(app.tr(" [Esc] cancel "), app.theme.warn()),
    ]);
    frame.render_widget(
        Paragraph::new(Text::from(vec![line, buttons])).block(w::panel(
            Line::from(format!(" {} ", app.tr(label))),
            app.theme,
        )),
        popup,
    );
}

fn prompt_popup_rect(area: Rect) -> Rect {
    w::centered(area, area.width.saturating_sub(4).clamp(8, 72), 4)
}

/// Which button in a text prompt was clicked.
pub(crate) fn prompt_choice_at(app: &App, column: u16, row: u16) -> Option<bool> {
    let popup = prompt_popup_rect(Rect::new(0, 0, app.viewport.0, app.viewport.1));
    if row != popup.y.saturating_add(2) {
        return None;
    }
    let start = popup.x.saturating_add(1);
    let accept = app.tr(" [Enter] accept ").width();
    let cancel = app.tr(" [Esc] cancel ").width();
    let x = usize::from(column.saturating_sub(start));
    if column < start {
        None
    } else if x < accept {
        Some(true)
    } else if (accept + 3..accept + 3 + cancel).contains(&x) {
        Some(false)
    } else {
        None
    }
}

/// A question about something destructive.
fn confirm(frame: &mut Frame<'_>, area: Rect, app: &App, question: &str, action: &crate::Action) {
    let question = app.format_status_text(question);
    let width = area.width.saturating_sub(4).clamp(12, 72);
    // Tall enough for the question at *this* width, because the question is
    // usually a name and names are as long as they are: the popup was five rows
    // and the buttons are the last of them, so a 44-character name pushed
    // `[y] yes [n] no` off the bottom — a confirmation with nothing to confirm
    // with. `+ 4` is the two borders, the blank line and the button row.
    let inner = usize::from(width).saturating_sub(2).max(1);
    let rows = wrapped_rows(&question, inner) + 4;
    let height = u16::try_from(rows)
        .unwrap_or(u16::MAX)
        .min(area.height)
        .max(5);
    let popup = w::centered(area, width, height);
    frame.render_widget(Clear, popup);
    let block = w::empty_panel(app.theme)
        .border_style(app.theme.error())
        .title(Line::from(format!(
            " {} ",
            crate::i18n::action_label(app.language(), action)
        )))
        .title_style(app.theme.error());
    frame.render_widget(block, popup);
    let inner = popup.inner(ratatui::layout::Margin::new(1, 1));
    let question_area = Rect::new(
        inner.x,
        inner.y,
        inner.width,
        inner.height.saturating_sub(2),
    );
    frame.render_widget(
        Paragraph::new(question)
            .style(app.theme.key_label())
            .wrap(Wrap { trim: true }),
        question_area,
    );
    let buttons = Line::from(vec![
        Span::styled(app.tr(" [y] yes "), app.theme.error()),
        Span::styled("   ", app.theme.key_label()),
        Span::styled(app.tr("[n] no "), app.theme.key_label()),
    ]);
    frame.render_widget(
        Paragraph::new(buttons),
        Rect::new(
            inner.x,
            inner.y.saturating_add(inner.height.saturating_sub(1)),
            inner.width,
            1,
        ),
    );
}

/// Which visible yes/no button was clicked in a confirmation popup.
pub(crate) fn confirm_choice_at(app: &App, question: &str, column: u16, row: u16) -> Option<bool> {
    let area = Rect::new(0, 0, app.viewport.0, app.viewport.1);
    let width = area.width.saturating_sub(4).clamp(12, 72);
    let inner = usize::from(width).saturating_sub(2).max(1);
    let rows = wrapped_rows(&app.format_status_text(question), inner);
    let height = u16::try_from(rows.saturating_add(4))
        .unwrap_or(u16::MAX)
        .min(area.height)
        .max(5);
    let popup = w::centered(area, width, height);
    if row != popup.y.saturating_add(popup.height.saturating_sub(2)) {
        return None;
    }
    let start = popup.x.saturating_add(1);
    let yes = app.tr(" [y] yes ").width();
    let no = app.tr("[n] no ").width();
    let x = usize::from(column.saturating_sub(start));
    if column < start {
        None
    } else if x < yes {
        Some(true)
    } else if (yes + 3..yes + 3 + no).contains(&x) {
        Some(false)
    } else {
        None
    }
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
    if title == crate::app::UPDATE_TITLE {
        application_update::render(frame, area, app, selected);
        return;
    }
    let width = area.width.saturating_sub(4).clamp(12, 72);
    let height = u16::try_from(items.len())
        .unwrap_or(u16::MAX)
        .saturating_add(2)
        .min(area.height);
    let popup = w::centered(area, width, height);
    frame.render_widget(Clear, popup);
    let rows: Vec<Line<'static>> = items
        .iter()
        .map(|item| Line::from(app.tr(item).to_owned()))
        .collect();
    let display_title = title
        .strip_prefix("setting:")
        .and_then(|key| {
            app.settings_rows
                .items()
                .iter()
                .find(|row| row.key == key)
                .map(|row| row.label)
        })
        .unwrap_or(title);
    let list = List::new(rows)
        .block(w::panel(
            Line::from(format!(" {} ", app.tr(display_title))),
            app.theme,
        ))
        .highlight_style(app.theme.selection())
        .highlight_symbol("▸ ");
    let mut state =
        ListState::default().with_selected(Some(selected.min(items.len().saturating_sub(1))));
    frame.render_stateful_widget(list, popup, &mut state);
}

/// A modal popup displaying a full status or error message.
fn message_popup(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    title: &str,
    text: &str,
    kind: crate::app::StatusKind,
    scroll: usize,
) {
    let popup = message_popup_rect(area, text);
    let border_style = match kind {
        crate::app::StatusKind::Error => app.theme.error(),
        crate::app::StatusKind::Warning => app.theme.warn(),
        crate::app::StatusKind::Success => app.theme.ok(),
        crate::app::StatusKind::Info => app.theme.info(),
    };
    let marker = match kind {
        crate::app::StatusKind::Error => "✗ ",
        crate::app::StatusKind::Warning => "! ",
        crate::app::StatusKind::Success => "✓ ",
        crate::app::StatusKind::Info => "i ",
    };

    frame.render_widget(Clear, popup);

    let block = w::empty_panel(app.theme)
        .border_style(border_style)
        .title(Line::from(format!(" {} ", app.tr(title))))
        .title_style(border_style)
        .title_bottom(Line::from(format!(
            " {} ",
            app.tr("j/k scroll · PgUp/PgDn page · Esc close")
        )));

    frame.render_widget(
        Paragraph::new(format!("{marker}{text}"))
            .style(app.theme.key_label())
            .wrap(Wrap { trim: true })
            .block(block)
            .scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0)),
        popup,
    );
}

fn message_popup_rect(area: Rect, text: &str) -> Rect {
    let max_width = (area.width.saturating_mul(75) / 100)
        .max(16)
        .min(area.width);
    let max_height = (area.height.saturating_mul(70) / 100)
        .max(4)
        .min(area.height);
    let content_width = text.lines().map(UnicodeWidthStr::width).max().unwrap_or(0);
    let width = u16::try_from(content_width.saturating_add(4))
        .unwrap_or(u16::MAX)
        .clamp(16.min(area.width), max_width);
    let inner_width = usize::from(width.saturating_sub(2)).max(1);
    let rows: usize = text
        .lines()
        .map(|line| wrapped_rows(line, inner_width))
        .sum();
    let height = u16::try_from(rows.saturating_add(2))
        .unwrap_or(u16::MAX)
        .clamp(4.min(area.height), max_height);
    w::centered(area, width, height)
}

/// Maximum vertical scroll for the full status message at the current size.
pub(crate) fn message_scroll_limit(viewport: (u16, u16), text: &str) -> usize {
    let popup = message_popup_rect(Rect::new(0, 0, viewport.0, viewport.1), text);
    let inner_width = usize::from(popup.width.saturating_sub(2)).max(1);
    let rows: usize = text
        .lines()
        .map(|line| wrapped_rows(line, inner_width))
        .sum();
    rows.saturating_sub(usize::from(popup.height.saturating_sub(2)))
}

/// The choice under a picker click. `ListState` starts at offset zero and
/// scrolls just enough to keep the selected item in its inner viewport.
pub(crate) fn picker_item_at(
    viewport: (u16, u16),
    items_len: usize,
    selected: usize,
    column: u16,
    row: u16,
) -> Option<usize> {
    let area = Rect::new(0, 0, viewport.0, viewport.1);
    let width = area.width.saturating_sub(4).clamp(12, 72);
    let height = u16::try_from(items_len)
        .unwrap_or(u16::MAX)
        .saturating_add(2)
        .min(area.height);
    let popup = w::centered(area, width, height);
    let inner = popup.inner(ratatui::layout::Margin::new(1, 1));
    if !inner.contains(ratatui::layout::Position::new(column, row)) || inner.height == 0 {
        return None;
    }
    let offset = selected.saturating_sub(usize::from(inner.height).saturating_sub(1));
    let index = offset + usize::from(row - inner.y);
    (index < items_len).then_some(index)
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
    let popup = preview_popup_rect(area, lines);
    frame.render_widget(Clear, popup);
    let text = Text::from(
        lines
            .iter()
            .map(|line| Line::from(line.clone()))
            .collect::<Vec<_>>(),
    );
    let block = w::panel(Line::from(format!(" {} ", app.tr(title))), app.theme).title_bottom(
        Line::from(format!(
            " {} ",
            app.tr("j/k scroll · PgUp/PgDn page · Esc close")
        ))
        .style(app.theme.key_label()),
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

fn preview_popup_rect(area: Rect, lines: &[String]) -> Rect {
    let max_width = area.width.saturating_mul(75) / 100;
    let max_height = area.height.saturating_mul(70) / 100;
    let content_width = lines
        .iter()
        .map(|line| UnicodeWidthStr::width(line.as_str()))
        .max()
        .unwrap_or(0);
    let width = u16::try_from(content_width.saturating_add(2))
        .unwrap_or(u16::MAX)
        .clamp(12.min(area.width), max_width.max(12).min(area.width));
    let inner_width = usize::from(width.saturating_sub(2)).max(1);
    let rows: usize = lines
        .iter()
        .map(|line| wrapped_rows(line, inner_width))
        .sum();
    let height = u16::try_from(rows.saturating_add(2))
        .unwrap_or(u16::MAX)
        .clamp(4.min(area.height), max_height.max(4).min(area.height));
    w::centered(area, width, height)
}

/// Visible text rows in the preview panel.
pub(crate) fn preview_page_rows(viewport: (u16, u16), lines: &[String]) -> usize {
    let popup = preview_popup_rect(Rect::new(0, 0, viewport.0, viewport.1), lines);
    usize::from(popup.height.saturating_sub(2)).max(1)
}

/// Maximum visual scroll after wrapping long fields, not merely line count.
pub(crate) fn preview_scroll_limit(viewport: (u16, u16), lines: &[String]) -> usize {
    let popup = preview_popup_rect(Rect::new(0, 0, viewport.0, viewport.1), lines);
    let inner_width = usize::from(popup.width.saturating_sub(2)).max(1);
    let rows: usize = lines
        .iter()
        .map(|line| wrapped_rows(line, inner_width))
        .sum();
    rows.saturating_sub(preview_page_rows(viewport, lines))
}

/// How many rows `text` needs at `width`, wrapped the way `Wrap` wraps it.
///
/// Greedy on whitespace, with a word longer than the line broken where it has
/// to be. Counting characters and dividing was the first attempt and it
/// under-counted: a name with spaces wraps at the last space that fits, so the
/// remainder of the line is wasted and the block is taller than the division
/// says. Twenty-nine of ninety names were still losing their buttons.
/// How many cells one character occupies.
///
/// Zero for a combining mark, which is the case that matters: it is a character
/// with no cell of its own, so anything that assumes at least one is wrong in
/// both directions — a window that is one character short, and a caret drawn in
/// a cell that does not exist.
fn cell_width(ch: char) -> usize {
    UnicodeWidthStr::width(ch.to_string().as_str())
}

pub(super) fn wrapped_rows(text: &str, width: usize) -> usize {
    let width = width.max(1);
    // Columns, not characters: a CJK message is twice as wide as its character
    // count says, and a terminal wraps on columns.
    let mut rows = 1;
    let mut used = 0;
    let mut breaks = 0;
    for ch in text.chars() {
        let wide = UnicodeWidthStr::width(ch.to_string().as_str()).max(1);
        if used + wide > width {
            rows += 1;
            used = 0;
        }
        used += wide;
        if ch == ' ' {
            breaks += 1;
        }
    }
    // Deliberately an **over**-estimate. `Wrap` breaks at the last space that
    // fits rather than at the column, so the real count is at least the
    // character count and at most that plus one row per space. Rounding the
    // other way lost the newest line of a log, which is the one line a
    // following pane must never lose; rounding this way shows one line fewer
    // than it could, which nobody notices.
    rows + breaks
}
