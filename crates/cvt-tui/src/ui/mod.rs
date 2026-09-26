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
        Screen::Profiles | Screen::Tests => 9,
        Screen::Proxies => 8,
        Screen::Connections | Screen::Rules => 7,
        Screen::Settings => 5,
        Screen::Home | Screen::Logs | Screen::Help => return None,
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
        Overlay::Message { title, text, kind } => {
            message_popup(frame, area, app, title, text, *kind);
        }
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
    let hint = app.tr(match kind {
        PromptKind::Search => "type to narrow the list · Enter keep · Esc clear",
        _ => "Enter accept · Esc cancel",
    });
    let block = w::panel(Line::from(format!(" {} ", app.tr(label))), app.theme)
        .title_bottom(Line::from(format!(" {hint} ")).style(app.theme.key_label()));
    frame.render_widget(Paragraph::new(line).block(block), popup);
}

/// A question about something destructive.
fn confirm(frame: &mut Frame<'_>, area: Rect, app: &App, question: &str, action: &crate::Action) {
    let width = area.width.saturating_sub(4).clamp(12, 72);
    // Tall enough for the question at *this* width, because the question is
    // usually a name and names are as long as they are: the popup was five rows
    // and the buttons are the last of them, so a 44-character name pushed
    // `[y] yes [n] no` off the bottom — a confirmation with nothing to confirm
    // with. `+ 4` is the two borders, the blank line and the button row.
    let inner = usize::from(width).saturating_sub(2).max(1);
    let rows = wrapped_rows(question, inner) + 4;
    let height = u16::try_from(rows)
        .unwrap_or(u16::MAX)
        .min(area.height)
        .max(5);
    let popup = w::centered(area, width, height);
    frame.render_widget(Clear, popup);
    let body = Text::from(vec![
        Line::from(Span::styled(question.to_owned(), app.theme.key_label())),
        Line::default(),
        Line::from(vec![
            Span::styled(app.tr(" [y] yes "), app.theme.error()),
            Span::styled("   ", app.theme.key_label()),
            Span::styled(app.tr("[n] no "), app.theme.key_label()),
        ]),
    ]);
    let block = w::empty_panel(app.theme)
        .border_style(app.theme.error())
        .title(Line::from(format!(
            " {} ",
            crate::i18n::action_label(app.language(), action)
        )))
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
    let rows: Vec<Line<'static>> = items
        .iter()
        .map(|item| Line::from(app.tr(item).to_owned()))
        .collect();
    let list = List::new(rows)
        .block(w::panel(
            Line::from(format!(" {} ", app.tr(title))),
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
) {
    let width = area.width.saturating_sub(4).clamp(16, 76);
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

    let body_lines: Vec<Line<'static>> = vec![
        Line::from(vec![
            Span::styled(marker, border_style),
            Span::styled(text.to_owned(), app.theme.key_label()),
        ]),
        Line::default(),
        Line::from(Span::styled(
            format!("[{}]", app.tr("Esc/Enter close")),
            app.theme.dim(),
        )),
    ];

    let height = u16::try_from(body_lines.len())
        .unwrap_or(u16::MAX)
        .saturating_add(4)
        .min(area.height)
        .max(6);

    let popup = w::centered(area, width, height);
    frame.render_widget(Clear, popup);

    let block = w::empty_panel(app.theme)
        .border_style(border_style)
        .title(Line::from(format!(" {} ", app.tr(title))))
        .title_style(border_style);

    frame.render_widget(
        Paragraph::new(Text::from(body_lines))
            .wrap(Wrap { trim: true })
            .block(block),
        popup,
    );
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
