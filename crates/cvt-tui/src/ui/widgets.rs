//! Pieces every screen draws with.
//!
//! Everything here takes a [`Theme`] and asks it for a *semantic* style, never
//! for a colour: that is what makes the monochrome theme a setting rather than
//! a second renderer. The helpers also share one rule for empty lists — say
//! something useful rather than drawing a blank box — because an empty panel
//! that looks the same as a panel that failed to load is the most common way
//! for a TUI to waste the user's time.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Flex, Layout, Rect};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Paragraph, Row, Table as TableWidget, TableState, Wrap};

use crate::action::Screen;
use crate::app::{App, StatusKind};
use crate::state::{Filterable, Table, human_bytes, human_delay};
use crate::theme::Theme;

/// A bordered panel with an emphasised title.
///
/// Rounded corners are used everywhere so that a frame reads as a frame and
/// never competes with the box-drawing characters of a table header.
#[must_use]
pub fn panel(title: impl Into<Line<'static>>, theme: Theme) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme.dim())
        .title(title.into())
        .title_style(theme.emphasis())
}

/// A bordered panel with no title.
#[must_use]
pub fn empty_panel(theme: Theme) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme.dim())
}

/// The rows a table currently shows, with the filter applied.
///
/// [`Table`] keeps its visible set private, so the renderer recomputes it with
/// the same predicate the table used. Indices therefore line up with
/// [`Table::selected_index`] and [`Table::offset`], which is what makes the
/// cursor land on the row the user is looking at.
#[must_use]
pub fn visible<T: Filterable>(table: &Table<T>) -> Vec<&T> {
    let needle = table.filter();
    table
        .items()
        .iter()
        .filter(|row| row.matches_filter(needle))
        .collect()
}

/// The widget state matching a table's cursor and scroll position.
#[must_use]
pub fn state_of<T: Filterable>(table: &Table<T>) -> TableState {
    let selected = if table.is_empty() {
        None
    } else {
        Some(table.selected_index())
    };
    TableState::new()
        .with_offset(table.offset())
        .with_selected(selected)
}

/// The cursor and scroll state for whichever table is on screen.
#[must_use]
pub fn active_state(app: &App) -> TableState {
    match app.screen {
        Screen::Profiles => state_of(&app.profiles),
        Screen::Rules => state_of(&app.rules),
        Screen::Tests => state_of(&app.tests),
        Screen::Settings => state_of(&app.settings_rows),
        Screen::Connections => state_of(&app.connections),
        _ => state_of(&app.nodes),
    }
}

/// A table of rows, ready to draw.
#[derive(Debug, Clone)]
pub struct ListSpec {
    /// Cursor and scroll position.
    pub state: TableState,
    /// The panel title.
    pub title: String,
    /// Column headings.
    pub header: Vec<&'static str>,
    /// Column widths.
    pub widths: Vec<Constraint>,
    /// The rows to show.
    pub rows: Vec<Row<'static>>,
    /// What to say when there is nothing to show.
    pub empty: String,
}

/// Draw a list, or a useful message when it has no rows.
pub fn list(frame: &mut Frame<'_>, area: Rect, app: &App, spec: ListSpec) {
    if spec.rows.is_empty() {
        message(frame, area, app.theme, app.tr(&spec.empty));
        return;
    }
    // The filter is reported here rather than by each screen: a list that is
    // hiding rows without saying so looks like a list that lost them.
    let filter = app.active_filter();
    let title = if filter.is_empty() {
        spec.title
    } else {
        format!("{}· {}: {filter} ", spec.title, app.tr("filter"))
    };
    let table = TableWidget::new(spec.rows, spec.widths)
        .header(
            Row::new(spec.header.into_iter().map(|heading| app.tr(heading)))
                .style(app.theme.emphasis()),
        )
        .block(panel(Line::from(title), app.theme))
        .column_spacing(1)
        .row_highlight_style(app.theme.selection())
        .highlight_symbol("");
    let mut state = spec.state;
    frame.render_stateful_widget(table, area, &mut state);
}

/// A centred message, used for every empty state.
pub fn message(frame: &mut Frame<'_>, area: Rect, theme: Theme, text: &str) {
    let paragraph = Paragraph::new(text)
        .style(theme.key_label())
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true })
        .block(empty_panel(theme));
    frame.render_widget(paragraph, area);
}

/// A pane of `label: value` rows, wrapped, with a title.
pub fn details(frame: &mut Frame<'_>, area: Rect, app: &App, title: &str, rows: &[(&str, String)]) {
    let theme = app.theme;
    let lines: Vec<Line<'static>> = rows
        .iter()
        .map(|(label, value)| {
            Line::from(vec![
                Span::styled(format!("{}: ", app.tr(label)), theme.dim()),
                Span::styled(value.clone(), theme.key_label()),
            ])
        })
        .collect();
    let paragraph = Paragraph::new(Text::from(lines))
        .wrap(Wrap { trim: false })
        .block(panel(format!(" {} ", app.tr(title.trim())), theme));
    frame.render_widget(paragraph, area);
}

/// The one-line core summary shown in the tab bar.
///
/// It belongs to the frame rather than to the dashboard: whether the core is
/// up changes what *every* other screen can do, so it has to be visible from
/// all of them.
#[must_use]
pub fn core_summary(app: &App) -> String {
    let mut parts = vec![crate::i18n::core_status(app.language(), &app.core)];
    if let Some(live) = &app.metrics.latest {
        parts.push(format!(
            "↓{}/s ↑{}/s",
            human_bytes(live.down_rate),
            human_bytes(live.up_rate)
        ));
    }
    if app.queued_tests() > 0 {
        parts.push(crate::i18n::message(
            app.language(),
            crate::i18n::Message::RunningTests(app.queued_tests()),
        ));
    }
    parts.join("  ")
}

/// The key hints that apply on the current screen.
///
/// The number shown is derived from the width, because a hint cut in half
/// tells the user less than not showing it at all.
#[must_use]
pub fn hints(app: &App, width: u16) -> Line<'static> {
    let limit = usize::from((width / 16).clamp(1, 8));
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (key, action) in app.keymap.hint_bindings(app.screen, limit) {
        spans.push(Span::styled(format!(" {key} "), app.theme.key_hint()));
        spans.push(Span::styled(
            format!("{} ", crate::i18n::action_label(app.language(), action)),
            app.theme.key_label(),
        ));
    }
    Line::from(spans)
}

/// The status message, styled by how bad it is.
#[must_use]
pub fn status_line(app: &App, width: u16) -> Line<'static> {
    let Some(status) = app.current_status() else {
        return Line::default();
    };
    let (marker, style) = match status.kind {
        StatusKind::Info => ("i", app.theme.info()),
        StatusKind::Success => ("✓", app.theme.ok()),
        StatusKind::Warning => ("!", app.theme.warn()),
        StatusKind::Error => ("✗", app.theme.error()),
    };
    let text = app.format_status_text(&status.text);
    let marker_str = format!(" {marker} ");
    let marker_width = unicode_width::UnicodeWidthStr::width(marker_str.as_str());
    let text_width = unicode_width::UnicodeWidthStr::width(text.as_str());
    let available = usize::from(width).saturating_sub(marker_width);

    let hint_str = app.tr_key(crate::i18n::TextKey::StatusOverflowMore);
    let hint_width = unicode_width::UnicodeWidthStr::width(hint_str);

    if text_width > available && available > hint_width {
        let max_content_width = available.saturating_sub(hint_width);
        let mut truncated = String::new();
        let mut cur_w = 0;
        for ch in text.chars() {
            let ch_w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if cur_w + ch_w > max_content_width {
                break;
            }
            cur_w += ch_w;
            truncated.push(ch);
        }
        truncated.push_str(hint_str);
        Line::from(vec![
            Span::styled(marker_str, style),
            Span::styled(truncated, style),
        ])
    } else {
        Line::from(vec![
            Span::styled(marker_str, style),
            Span::styled(text, style),
        ])
    }
}

/// A rectangle of `width` by `height`, centred in `area` and never larger.
#[must_use]
pub fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let horizontal = Layout::horizontal([Constraint::Length(width.min(area.width))])
        .flex(Flex::Center)
        .split(area);
    let vertical = Layout::vertical([Constraint::Length(height.min(area.height))])
        .flex(Flex::Center)
        .split(horizontal[0]);
    vertical[0]
}

/// Split an area into a list and its detail pane.
///
/// On a terminal too short for both, the list keeps everything: it is the part
/// the user navigates, and a detail pane without a list is unreadable.
#[must_use]
pub fn list_and_detail(area: Rect, detail_height: u16) -> (Rect, Rect) {
    let detail = if area.height > detail_height + 3 {
        detail_height
    } else {
        0
    };
    let chunks = Layout::vertical([Constraint::Min(0), Constraint::Length(detail)]).split(area);
    (chunks[0], chunks[1])
}

/// A latency reading, styled by how usable it is.
#[must_use]
pub fn delay_span(delay: Option<u16>, theme: Theme) -> Span<'static> {
    Span::styled(human_delay(delay), theme.delay(delay))
}
