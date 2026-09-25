//! Colours and styles.
//!
//! One palette, consulted by every renderer, so that a change to how errors
//! look happens in one place. Styles are built from *semantic* roles rather
//! than literal colours: a renderer asks for [`Theme::error`], never for red,
//! which is what makes a monochrome or colour-blind-safe palette a
//! configuration change rather than a rewrite.
//!
//! Every style degrades safely on a terminal without colour: nothing relies on
//! colour alone to carry meaning, and [`Theme::monochrome`] removes it
//! entirely.

use ratatui::style::{Modifier, Style};

/// Colours and styles used across the interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    /// Whether colour is used at all.
    pub color: bool,
}

impl Default for Theme {
    fn default() -> Self {
        Self { color: true }
    }
}

impl Theme {
    /// A theme that uses no colour, only attributes.
    #[must_use]
    pub fn monochrome() -> Self {
        Self { color: false }
    }

    /// Build a theme from the colour setting.
    #[must_use]
    pub fn from_settings(color: bool) -> Self {
        Self { color }
    }

    fn colour(self, c: ratatui::style::Color) -> Style {
        if self.color {
            Style::default().fg(c)
        } else {
            Style::default()
        }
    }

    /// The selected row in a list or table.
    #[must_use]
    pub fn selection(&self) -> Style {
        if self.color {
            Style::default()
                .bg(ratatui::style::Color::Indexed(237))
                .fg(ratatui::style::Color::Indexed(231))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        }
    }

    /// Secondary text: units, counts, timestamps.
    #[must_use]
    pub fn dim(&self) -> Style {
        self.colour(ratatui::style::Color::DarkGray)
    }

    /// Emphasised text.
    #[must_use]
    pub fn emphasis(&self) -> Style {
        let s = self.colour(ratatui::style::Color::Cyan);
        s.add_modifier(Modifier::BOLD)
    }

    /// A successful result.
    #[must_use]
    pub fn ok(&self) -> Style {
        self.colour(ratatui::style::Color::Green)
    }

    /// A warning.
    #[must_use]
    pub fn warn(&self) -> Style {
        self.colour(ratatui::style::Color::Yellow)
    }

    /// An error, or a destructive action.
    #[must_use]
    pub fn error(&self) -> Style {
        self.colour(ratatui::style::Color::Red)
    }

    /// An informational note.
    #[must_use]
    pub fn info(&self) -> Style {
        self.colour(ratatui::style::Color::Blue)
    }

    /// How a latency reading is coloured, by how usable it is.
    ///
    /// The thresholds are the ones every dashboard uses, so they will match
    /// the user's expectation: under 200 ms reads as fast, under 500 ms as
    /// acceptable, and anything above or absent as a problem.
    #[must_use]
    pub fn delay(self, millis: Option<u16>) -> Style {
        match millis {
            // Zero is what the core reports before a node has been measured,
            // which is not the same thing as a node that failed.
            Some(0) => self.dim(),
            Some(d) if d < 200 => self.ok(),
            Some(d) if d < 500 => self.warn(),
            _ => self.error(),
        }
    }

    /// The colour a traffic meter uses.
    #[must_use]
    pub fn traffic(&self) -> Style {
        self.colour(ratatui::style::Color::Magenta)
    }

    /// A key hint in the footer.
    #[must_use]
    pub fn key_hint(&self) -> Style {
        let s = self.colour(ratatui::style::Color::Yellow);
        s.add_modifier(Modifier::BOLD)
    }

    /// The label following a key hint.
    #[must_use]
    pub fn key_label(&self) -> Style {
        self.dim()
    }

    /// The tab bar's active tab.
    #[must_use]
    pub fn tab_active(&self) -> Style {
        let s = self.colour(ratatui::style::Color::Green);
        s.add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
    }

    /// A log line's style, by severity.
    #[must_use]
    pub fn log(&self, level: &str) -> Style {
        match level.to_ascii_lowercase().as_str() {
            "error" => self.error(),
            "warning" | "warn" => self.warn(),
            "debug" => self.dim(),
            _ => Style::default(),
        }
    }

    /// The style for a disabled or skipped row.
    #[must_use]
    pub fn disabled(&self) -> Style {
        let s = self.dim();
        s.add_modifier(Modifier::CROSSED_OUT)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    #[test]
    fn monochrome_uses_no_colour_anywhere() {
        let t = Theme::monochrome();
        for style in [
            t.selection(),
            t.dim(),
            t.emphasis(),
            t.ok(),
            t.warn(),
            t.error(),
            t.info(),
            t.traffic(),
            t.key_hint(),
            t.key_label(),
            t.tab_active(),
            t.log("error"),
            t.disabled(),
        ] {
            assert_eq!(
                style.fg, None,
                "no foreground colour in monochrome: {style:?}"
            );
            assert_eq!(
                style.bg, None,
                "no background colour in monochrome: {style:?}"
            );
        }
        // Selection still has to be visible without colour.
        assert!(t.selection().add_modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn colour_theme_actually_sets_colours() {
        let t = Theme::default();
        assert_eq!(t.error().fg, Some(Color::Red));
        assert_eq!(t.ok().fg, Some(Color::Green));
        assert_eq!(t.warn().fg, Some(Color::Yellow));
        assert_ne!(
            t.selection().bg,
            None,
            "selection needs a visible background"
        );
    }

    #[test]
    fn latency_thresholds_match_the_conventional_ones() {
        let t = Theme::default();
        assert_eq!(t.delay(Some(50)), t.ok());
        assert_eq!(t.delay(Some(199)), t.ok());
        assert_eq!(t.delay(Some(200)), t.warn());
        assert_eq!(t.delay(Some(499)), t.warn());
        assert_eq!(t.delay(Some(500)), t.error());
        assert_eq!(
            t.delay(None),
            t.error(),
            "an unmeasured node is a problem, not a zero"
        );
        assert_eq!(t.delay(Some(0)), t.dim(), "zero means not yet measured");
    }

    #[test]
    fn log_styles_track_severity() {
        let t = Theme::default();
        assert_eq!(t.log("error"), t.error());
        assert_eq!(
            t.log("ERROR"),
            t.error(),
            "the comparison is case-insensitive"
        );
        assert_eq!(t.log("warning"), t.warn());
        assert_eq!(t.log("warn"), t.warn(), "cores emit both spellings");
        assert_eq!(t.log("debug"), t.dim());
        assert_eq!(t.log("info"), Style::default());
        assert_eq!(t.log("nonsense-that-a-future-core-added"), Style::default());
    }

    #[test]
    fn from_settings_carries_the_flag() {
        assert!(!Theme::from_settings(false).color);
        assert!(Theme::from_settings(true).color);
    }
}
