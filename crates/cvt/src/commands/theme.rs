//! `theme` — the key map, as a plain-text reference.
//!
//! The key map lives in `cvt-tui`, where it is exercised by a terminal. This
//! command prints the same bindings without one, so the map can be read (and
//! diffed, and checked) from a script or a machine with no TTY.

use anyhow::Result;
use cvt_tui::keys::contexts_for;
use cvt_tui::{Context, Keymap, Screen};
use serde::Serialize;
use std::fmt::Write as _;
use unicode_width::UnicodeWidthStr;

use crate::context::Ctx;
use crate::output::{Output, Report};

/// Print the tabs and the key bindings.
///
/// # Errors
/// Only when the report cannot be written.
pub fn run(ctx: &Ctx) -> Result<()> {
    ctx.out().emit(&ThemeReport::gather())
}

/// One tab.
#[derive(Debug, Serialize)]
pub struct ScreenRow {
    /// Position in tab order, starting at 1.
    pub index: usize,
    /// The digit that jumps straight to it.
    pub digit: String,
    /// The tab label.
    pub title: &'static str,
}

/// One key binding.
#[derive(Debug, Serialize)]
pub struct BindingRow {
    /// Screen the binding applies on, or `anywhere`.
    pub screen: &'static str,
    /// Context the binding lives in.
    pub context: &'static str,
    /// How the key is written.
    pub keys: &'static str,
    /// What the key does.
    pub action: &'static str,
    /// A longer explanation.
    pub help: &'static str,
}

/// Everything `theme` prints.
#[derive(Debug, Serialize)]
pub struct ThemeReport {
    /// The tabs, in order.
    pub screens: Vec<ScreenRow>,
    /// Every binding, grouped by screen and context.
    pub bindings: Vec<BindingRow>,
}

impl ThemeReport {
    /// Read the key map out of `cvt-tui`.
    #[must_use]
    pub fn gather() -> Self {
        let keymap = Keymap::new();
        let mut screens = Vec::new();
        let mut bindings = Vec::new();

        for (index, screen) in Screen::all().into_iter().enumerate() {
            let position = index + 1;
            screens.push(ScreenRow {
                index: position,
                digit: position.to_string(),
                title: screen.title(),
            });
            // Global bindings are listed once, at the end: repeating them
            // under every tab would bury the ones that are tab-specific.
            for context in contexts_for(screen) {
                if context == Context::Global {
                    continue;
                }
                collect(&keymap, context, screen.title(), &mut bindings);
            }
        }
        collect(&keymap, Context::Global, "anywhere", &mut bindings);

        Self { screens, bindings }
    }

    /// The bindings that apply on one screen, in the order they are listed.
    fn for_screen(&self, title: &str) -> Vec<&BindingRow> {
        self.bindings
            .iter()
            .filter(|row| row.screen == title)
            .collect()
    }
}

fn collect(keymap: &Keymap, context: Context, screen: &'static str, out: &mut Vec<BindingRow>) {
    for binding in keymap
        .bindings()
        .iter()
        .filter(|binding| binding.context == context && !binding.display.is_empty())
    {
        out.push(BindingRow {
            screen,
            context: context.label(),
            keys: binding.display,
            action: binding.action.label(),
            help: binding.action.help(),
        });
    }
}

impl Report for ThemeReport {
    fn schema(&self) -> &'static str {
        "cvt.theme.v1"
    }

    fn render(&self, _out: Output) -> String {
        let mut text = String::from("tabs\n");
        for screen in &self.screens {
            let _ = writeln!(text, "  {}  {}", screen.digit, screen.title);
        }

        for screen in &self.screens {
            let rows = self.for_screen(screen.title);
            if rows.is_empty() {
                continue;
            }
            let _ = writeln!(text, "\n{}", screen.title.to_lowercase());
            text.push_str(&render_bindings(&rows));
        }
        let global = self.for_screen("anywhere");
        if !global.is_empty() {
            text.push_str("\nanywhere\n");
            text.push_str(&render_bindings(&global));
        }
        text.trim_end().to_owned()
    }
}

fn render_bindings(rows: &[&BindingRow]) -> String {
    let width = rows
        .iter()
        .map(|row| UnicodeWidthStr::width(row.keys))
        .max()
        .unwrap_or(0);
    let mut text = String::new();
    for row in rows {
        let pad = width.saturating_sub(UnicodeWidthStr::width(row.keys));
        let _ = writeln!(
            text,
            "  {}{}  {}  ({})",
            row.keys,
            " ".repeat(pad),
            row.action,
            row.help
        );
    }
    text
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn every_tab_is_listed_with_the_digit_that_reaches_it() {
        let report = ThemeReport::gather();
        assert_eq!(report.screens.len(), 9);
        assert_eq!(report.screens[0].digit, "1");
        assert_eq!(report.screens[0].title, "Home");
        assert_eq!(report.screens[8].digit, "9");
        assert_eq!(report.screens[8].title, "Help");
    }

    #[test]
    fn the_bindings_are_the_key_maps_own_and_every_one_has_a_key() {
        let report = ThemeReport::gather();
        let keymap = Keymap::new();
        assert!(!report.bindings.is_empty());
        assert!(
            report.bindings.iter().all(|row| !row.keys.is_empty()),
            "a binding with no key cannot be pressed"
        );
        // Everything listed comes from the key map: a binding that is not in
        // it would be an invention of this report.
        for row in &report.bindings {
            assert!(
                keymap
                    .bindings()
                    .iter()
                    .any(|binding| binding.display == row.keys
                        && binding.action.label() == row.action),
                "`{}` is not in the key map",
                row.keys
            );
        }
    }

    #[test]
    fn a_screen_specific_binding_is_listed_under_its_own_screen() {
        let report = ThemeReport::gather();
        let profiles: Vec<&BindingRow> = report.for_screen("Profiles");
        assert!(
            profiles.iter().any(|row| row.action == "update"),
            "the profiles tab lists its own actions: {:?}",
            profiles.iter().map(|r| r.action).collect::<Vec<_>>()
        );
        assert!(
            !report
                .for_screen("Logs")
                .iter()
                .any(|row| row.action == "update"),
            "and not under a screen it does not apply to"
        );
    }

    #[test]
    fn global_bindings_are_listed_once_rather_than_under_every_tab() {
        let report = ThemeReport::gather();
        let quits: Vec<&BindingRow> = report
            .bindings
            .iter()
            .filter(|row| row.keys == "q")
            .collect();
        assert_eq!(quits.len(), 1, "the quit binding appears once");
        assert_eq!(quits[0].screen, "anywhere");
        assert_eq!(quits[0].context, "anywhere");
    }

    #[test]
    fn the_plain_text_reference_names_the_tabs_and_the_keys() {
        let report = ThemeReport::gather();
        let text = report.render(Output::new(false, 0, false));
        assert!(text.starts_with("tabs\n"), "{text}");
        assert!(text.contains("  1  Home"), "{text}");
        assert!(text.contains("\nprofiles\n"), "{text}");
        assert!(text.contains("\nanywhere\n"), "{text}");
        assert!(text.contains("Tab"), "the tab binding is listed: {text}");
    }

    #[test]
    fn the_json_shape_carries_the_help_text_the_plain_form_omits() {
        let report = ThemeReport::gather();
        let value = crate::output::to_value(&report).unwrap();
        assert_eq!(value["schema"], serde_json::json!("cvt.theme.v1"));
        assert_eq!(value["screens"][0]["title"], serde_json::json!("Home"));
        let first = &value["bindings"][0];
        assert!(first["keys"].is_string());
        assert!(first["action"].is_string());
        assert!(first["help"].is_string());
        assert!(first["context"].is_string());
    }
}
