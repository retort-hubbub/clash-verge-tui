//! The live log stream.
//!
//! A log view is read from the bottom, which is why following is the default
//! and why stopping it freezes the *window* rather than the buffer: lines keep
//! arriving, the title reports how many are hidden, and turning follow back on
//! shows all of them at once.
//!
//! The level shown here is the level requested from the core, so what the user
//! sees and what the core emits cannot drift apart.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use crate::app::App;
use crate::ui::widgets as w;

/// Draw the logs screen.
pub fn render(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let block = w::panel(Line::from(title(app)), app.theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lines = visible_lines(app, inner.width, inner.height);
    if lines.is_empty() {
        let text = if app.logs.is_empty() {
            "no log lines yet — start the core, and check that stream.logs is on"
        } else {
            "no buffered line matches the filter"
        };
        frame.render_widget(
            Paragraph::new(app.tr(text))
                .style(app.theme.key_label())
                .wrap(Wrap { trim: true }),
            inner,
        );
        return;
    }
    // The window already ends where following stopped, so the paragraph is
    // rendered as it is: nothing here may re-anchor it to the newest line.
    let rendered: Vec<Line<'static>> = lines.iter().map(|line| render_line(line, app)).collect();
    frame.render_widget(Paragraph::new(rendered).wrap(Wrap { trim: false }), inner);
}

fn visible_lines(app: &App, width: u16, height: u16) -> Vec<&crate::row::LogRow> {
    let height = usize::from(height);
    let width = usize::from(width).max(1);
    // The window is in terminal rows. Count wrapped rows with the same
    // Paragraph implementation that draws them; a conservative estimate that
    // charged every space as a new row hid most of the buffer.
    let (candidates, _) = app.log_window(height.saturating_mul(4).max(height));
    let mut used = 0;
    let mut first = candidates.len();
    for line in candidates.iter().rev() {
        // Measure the styled line that will actually be drawn.
        let rows = Paragraph::new(render_line(line, app))
            .wrap(Wrap { trim: false })
            .line_count(u16::try_from(width).unwrap_or(u16::MAX));
        // At least the newest line, always. A single line taller than the pane
        // is clipped by the `Paragraph`, which shows its beginning — better
        // than the empty state, which is where the over-estimate below sent it
        // when one line exceeded the height on its own.
        if used + rows > height && first < candidates.len() {
            break;
        }
        used += rows;
        first -= 1;
    }
    candidates[first..].to_vec()
}

/// Resolve the log line under the pointer using the same wrapping as the renderer.
pub(crate) fn line_at(app: &App, column: u16, row: u16) -> Option<crate::row::LogRow> {
    let area = Rect::new(0, 1, app.viewport.0, app.viewport.1.saturating_sub(2));
    let inner = area.inner(ratatui::layout::Margin::new(1, 1));
    if !inner.contains(ratatui::layout::Position::new(column, row)) {
        return None;
    }
    let mut top = inner.y;
    for line in visible_lines(app, inner.width, inner.height) {
        let rows = Paragraph::new(render_line(line, app))
            .wrap(Wrap { trim: false })
            .line_count(inner.width.max(1));
        let bottom = top.saturating_add(u16::try_from(rows).unwrap_or(u16::MAX));
        if (top..bottom).contains(&row) {
            return Some(line.clone());
        }
        top = bottom;
    }
    None
}

fn render_line(line: &crate::row::LogRow, app: &App) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{} ", line.at), app.theme.dim()),
        Span::styled(format!("{:<7} ", line.level), app.theme.log(&line.level)),
        Span::styled(line.message.clone(), app.theme.key_label()),
    ])
}

/// The panel title: level, follow state, size and filter.
fn title(app: &App) -> String {
    let follow = if app.logs.follow {
        "following"
    } else {
        "frozen"
    };
    let hidden = app.log_window(0).1;
    let mut parts = vec![
        app.tr("logs").to_owned(),
        app.log_level.as_str().to_owned(),
        app.tr(follow).to_owned(),
        crate::i18n::message(
            app.language(),
            crate::i18n::Message::LogLines(app.logs.len()),
        ),
    ];
    if hidden > 0 {
        parts.push(crate::i18n::message(
            app.language(),
            crate::i18n::Message::LogNew(hidden),
        ));
    }
    if app.logs.dropped() > 0 {
        parts.push(crate::i18n::message(
            app.language(),
            crate::i18n::Message::LogDropped(app.logs.dropped()),
        ));
    }
    if !app.logs.filter().is_empty() {
        parts.push(format!("{}: {}", app.tr("filter"), app.logs.filter()));
    }
    format!(" {} ", parts.join(" · "))
}
