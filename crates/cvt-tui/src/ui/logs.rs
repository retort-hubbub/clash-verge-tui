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

    let height = usize::from(inner.height);
    let width = usize::from(inner.width).max(1);
    // The window is in *rows*, not lines. A line that wraps takes more than one
    // row, so asking for `height` lines and letting the `Paragraph` wrap them
    // made the pane taller than its area — and the `Paragraph` clips the
    // bottom, so the newest lines were the ones lost. Following a log is the
    // one thing this pane is for, so the window is taken generously and then
    // trimmed from the front until it fits.
    let (candidates, _) = app.log_window(height.saturating_mul(4).max(height));
    let mut used = 0;
    let mut first = candidates.len();
    for line in candidates.iter().rev() {
        // What is measured is what is *drawn*: `render_line` pads the level to
        // seven columns, so measuring the unpadded form under-counted every row
        // and the pane kept the wrong end of the window.
        let full = format!("{} {:<7} {}", line.at, line.level, line.message);
        let rows = super::wrapped_rows(&full, width);
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
    let lines = &candidates[first..];
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
    // The filter is reported by the list widget, so it is not repeated here.
    format!(" {} ", parts.join(" · "))
}
