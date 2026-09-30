//! Release summaries and three explicit update choices share drawing/hit geometry.
use super::widgets as w;
use crate::app::App;
use crate::i18n::TextKey;
use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span},
    widgets::{Clear, Paragraph, Wrap},
};

fn popup(area: Rect) -> Rect {
    w::centered(
        area,
        area.width.saturating_mul(4) / 5,
        (area.height.saturating_mul(3) / 4).min(22),
    )
}

pub(super) fn render(frame: &mut Frame<'_>, area: Rect, app: &App, selected: usize) {
    let Some(release) = &app.app_update else {
        return;
    };
    let popup = popup(area);
    frame.render_widget(Clear, popup);
    let block = w::panel(
        format!(
            " {} · {} ",
            app.tr_key(TextKey::ApplicationUpdate),
            release.tag
        ),
        app.theme,
    );
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let text = format!(
        "{} → {}\n{}\n\n{}",
        env!("CARGO_PKG_VERSION"),
        release.tag,
        release.url,
        release.summary
    );
    let description = Rect {
        height: inner.height.saturating_sub(4),
        ..inner
    };
    frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), description);
    for (index, key) in [
        TextKey::UpdateNow,
        TextKey::UpdateLater,
        TextKey::UpdateSkip,
    ]
    .into_iter()
    .enumerate()
    {
        let row = inner.y + inner.height.saturating_sub(3) + u16::try_from(index).unwrap_or(0);
        if row >= inner.y + inner.height {
            break;
        }
        let style = if selected == index {
            app.theme.selection()
        } else {
            app.theme.key_label()
        };
        let label = format!(
            "{} {}",
            if selected == index { "›" } else { " " },
            app.tr_key(key)
        );
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(label, style))),
            Rect::new(inner.x, row, inner.width, 1),
        );
    }
}

pub(crate) fn choice_at(viewport: (u16, u16), column: u16, row: u16) -> Option<usize> {
    let popup = popup(Rect::new(0, 0, viewport.0, viewport.1));
    let inner = Rect::new(
        popup.x.saturating_add(1),
        popup.y.saturating_add(1),
        popup.width.saturating_sub(2),
        popup.height.saturating_sub(2),
    );
    let start = inner.y + inner.height.saturating_sub(3);
    if inner.width == 0
        || column < inner.x
        || column >= inner.x + inner.width
        || row < start
        || row >= inner.y + inner.height
    {
        return None;
    }
    Some(usize::from(row - start))
}
