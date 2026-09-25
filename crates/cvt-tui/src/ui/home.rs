//! The dashboard.
//!
//! Everything here answers one question — is the core doing what I asked? —
//! so the screen is built around the supervisor's state, the two throughput
//! gauges, and the counters that make a misconfiguration obvious (no
//! connections, no memory reading, no samples at all).

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Gauge, Paragraph, Sparkline};

use crate::app::App;
use crate::state::{human_bytes, human_rate};
use crate::ui::widgets as w;

/// Draw the dashboard.
pub fn render(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let chunks = Layout::vertical([
        Constraint::Length(4),
        Constraint::Length(8),
        Constraint::Min(4),
        Constraint::Length(5),
    ])
    .split(area);
    status(frame, chunks[0], app);
    throughput(frame, chunks[1], app);
    readings(frame, chunks[2], app);
    attention(frame, chunks[3], app);
}

/// Where the core is, which binary it is, and where the data lives.
fn status(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let rows = [
        ("state", app.core.label()),
        (
            "version",
            app.version.clone().unwrap_or_else(|| "unknown".to_owned()),
        ),
        ("home", app.home.display().to_string()),
    ];
    w::details(frame, area, app.theme, " core ", &rows);
}

/// Traffic: a sparkline of recent samples and a gauge against the peak.
fn throughput(frame: &mut Frame<'_>, area: Rect, app: &App) {
    if app.metrics.is_empty() {
        w::message(
            frame,
            area,
            app.theme,
            "no traffic samples yet — start the core, and check that stream.traffic is on",
        );
        return;
    }
    let block = w::panel(" throughput ", app.theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(inner);

    let live = app.metrics.latest.unwrap_or_default();
    let summary = format!(
        "↓ {}   ↑ {}   total {}   {} connection(s)",
        human_rate(live.down_rate),
        human_rate(live.up_rate),
        human_bytes(live.total()),
        live.connections
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(summary, app.theme.traffic()))),
        rows[0],
    );

    let samples: Vec<u64> = app.metrics.down.iter().copied().collect();
    frame.render_widget(
        Sparkline::default()
            .data(samples)
            .style(app.theme.traffic()),
        rows[1],
    );

    let peak_down = app.metrics.down.iter().copied().max().unwrap_or(0);
    let peak_up = app.metrics.up.iter().copied().max().unwrap_or(0);
    gauge(
        frame,
        rows[2],
        app,
        "down",
        live.down_rate,
        peak_down,
        app.theme.traffic(),
    );
    gauge(
        frame,
        rows[3],
        app,
        "up",
        live.up_rate,
        peak_up,
        app.theme.info(),
    );
}

/// One direction, as a share of the busiest sample in the window.
///
/// A share rather than an absolute value because a rate has no ceiling: what
/// the user wants to see is the shape of the traffic, not a percentage of an
/// invented limit.
fn gauge(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    label: &str,
    rate: u64,
    peak: u64,
    style: ratatui::style::Style,
) {
    if area.is_empty() {
        return;
    }
    #[allow(clippy::cast_precision_loss)] // display only
    let ratio = if peak == 0 {
        0.0
    } else {
        (rate as f64 / peak as f64).clamp(0.0, 1.0)
    };
    let ratio = if ratio.is_finite() { ratio } else { 0.0 };
    let bar = Gauge::default()
        .ratio(ratio)
        .label(format!("{label} {}", human_rate(rate)))
        .gauge_style(style)
        .style(app.theme.dim());
    frame.render_widget(bar, area);
}

/// The counters that are easy to forget to check.
fn readings(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let live = app.metrics.latest.unwrap_or_default();
    let mut rows = vec![
        ("connections", live.connections.to_string()),
        ("memory", human_bytes(live.memory)),
        ("downloaded", human_bytes(live.down_total)),
        ("uploaded", human_bytes(live.up_total)),
        ("samples", app.metrics.len().to_string()),
    ];
    if app.queued_tests() > 0 {
        rows.push(("tests", format!("{} queued or running", app.queued_tests())));
    }
    if let Some(preview) = &app.preview {
        rows.push(("last preview", preview.summary.clone()));
    }
    let filter = app.active_filter();
    if !filter.is_empty() {
        rows.push(("filter", filter));
    }
    w::details(frame, area, app.theme, " readings ", &rows);
}

/// The things that are wrong, or that will surprise the user later.
///
/// The footer already names the keys, so this pane answers a different
/// question: what is stopping this installation from working the way it looks
/// like it should?
fn attention(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    match app.core {
        cvt_core::mihomo::supervisor::CoreStatus::NotInstalled => lines.push(Line::from(vec![
            Span::styled("no mihomo binary ", app.theme.error()),
            Span::styled(
                "— set core.binary in Settings, or put one in the core directory",
                app.theme.key_label(),
            ),
        ])),
        cvt_core::mihomo::supervisor::CoreStatus::StalePid { pid } => lines.push(Line::from(vec![
            Span::styled(
                format!("pid {pid} was recorded but does not answer "),
                app.theme.warn(),
            ),
            Span::styled("— start the core again", app.theme.key_label()),
        ])),
        cvt_core::mihomo::supervisor::CoreStatus::Running { .. } => {}
        cvt_core::mihomo::supervisor::CoreStatus::Stopped => lines.push(Line::from(vec![
            Span::styled("the core is not running ", app.theme.warn()),
            Span::styled("— press s to start it", app.theme.key_label()),
        ])),
    }
    if app.profiles.total() == 0 {
        lines.push(Line::from(vec![
            Span::styled("no profiles yet ", app.theme.warn()),
            Span::styled(
                "— press 2, then a, to add a subscription",
                app.theme.key_label(),
            ),
        ]));
    }
    if app.settings.streams_disabled() {
        lines.push(Line::from(vec![
            Span::styled("every live stream is switched off ", app.theme.warn()),
            Span::styled("— the dashboard will stay empty", app.theme.key_label()),
        ]));
    }
    if app.settings.ui.show_footer && app.current_status().is_none() && lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "nothing needs attention",
            app.theme.dim(),
        )));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "press ? for every key",
            app.theme.dim(),
        )));
    }
    let height = usize::from(area.height.saturating_sub(2));
    lines.truncate(height);
    frame.render_widget(
        Paragraph::new(Text::from(lines)).block(w::panel(" attention ", app.theme)),
        area,
    );
}
