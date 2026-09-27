//! The key map.
//!
//! Bindings are data, not `match` arms, so that three things which must agree
//! are derived from one list: what a key does, the hints in the footer, and
//! the reference on the help screen.
//!
//! # Resolution
//!
//! A key press is resolved against the contexts that apply to the current
//! screen, **most specific first**. That is what allows `s` to mean "start the
//! core" on the dashboard and "save" on the settings screen without either
//! binding being a special case. Two tests enforce the properties that make
//! this safe: no key is bound twice within one context, and every action is
//! reachable from somewhere.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::action::{Action, Screen};

/// Where a binding applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Context {
    /// Applies on every screen.
    Global,
    /// Applies wherever a list has a cursor.
    List,
    /// Dashboard only.
    Home,
    /// Profiles only.
    Profiles,
    /// Proxies only.
    Proxies,
    /// Connections only.
    Connections,
    /// Logs only.
    Logs,
    /// Rules only.
    Rules,
    /// Tests only.
    Tests,
    /// Settings only.
    Settings,
}

impl Context {
    /// A label for the footer legend.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Global => "anywhere",
            Self::List => "lists",
            Self::Home => "home",
            Self::Profiles => "profiles",
            Self::Proxies => "proxies",
            Self::Connections => "connections",
            Self::Logs => "logs",
            Self::Rules => "rules",
            Self::Tests => "tests",
            Self::Settings => "settings",
        }
    }
}

/// One key binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    /// The key.
    pub key: KeyCode,
    /// Modifiers that must be held.
    pub mods: KeyModifiers,
    /// What it does.
    pub action: Action,
    /// Where it applies.
    pub context: Context,
    /// How the key is written in the footer.
    pub display: &'static str,
}

impl Binding {
    const fn new(
        key: KeyCode,
        mods: KeyModifiers,
        action: Action,
        context: Context,
        display: &'static str,
    ) -> Self {
        Self {
            key,
            mods,
            action,
            context,
            display,
        }
    }

    /// Whether this binding matches a key press.
    #[must_use]
    pub fn matches(&self, event: KeyEvent) -> bool {
        // Compare only the modifiers we care about; a terminal may report
        // extra ones (notably SHIFT on an uppercase letter) that should not
        // make a binding unreachable.
        let relevant = KeyModifiers::CONTROL | KeyModifiers::ALT;
        self.key == event.code && (self.mods & relevant) == (event.modifiers & relevant)
    }
}

/// The complete key map.
#[derive(Debug, Clone)]
pub struct Keymap {
    bindings: Vec<Binding>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self::new()
    }
}

/// The contexts that apply on a screen, most specific first.
#[must_use]
pub fn contexts_for(screen: Screen) -> Vec<Context> {
    match screen {
        Screen::Home => vec![Context::Home, Context::Global],
        Screen::Profiles => vec![Context::Profiles, Context::List, Context::Global],
        Screen::Proxies => vec![Context::Proxies, Context::List, Context::Global],
        Screen::Connections => vec![Context::Connections, Context::List, Context::Global],
        Screen::Logs => vec![Context::Logs, Context::Global],
        Screen::Rules => vec![Context::Rules, Context::List, Context::Global],
        Screen::Tests => vec![Context::Tests, Context::List, Context::Global],
        Screen::Settings => vec![Context::Settings, Context::List, Context::Global],
        Screen::Help => vec![Context::Global],
    }
}

impl Keymap {
    /// The default key map.
    #[must_use]
    #[allow(clippy::too_many_lines)] // one line per binding is the readable form
    pub fn new() -> Self {
        use Action as A;
        use Context as C;
        use KeyCode as K;
        let ctrl = KeyModifiers::CONTROL;
        let none = KeyModifiers::NONE;
        let mut bindings = vec![
            // -- global ---------------------------------------------------
            Binding::new(K::Char('q'), none, A::Quit, C::Global, "q"),
            Binding::new(K::Char('c'), ctrl, A::Quit, C::Global, "C-c"),
            Binding::new(K::Char('?'), none, A::Goto(Screen::Help), C::Global, "?"),
            Binding::new(K::Tab, none, A::NextScreen, C::Global, "Tab"),
            Binding::new(K::BackTab, none, A::PreviousScreen, C::Global, "S-Tab"),
            Binding::new(K::Esc, none, A::Cancel, C::Global, "Esc"),
            Binding::new(K::Char('r'), none, A::Refresh, C::Global, "r"),
            Binding::new(K::Char('m'), none, A::ShowLastMessage, C::Global, "m"),
            // -- list movement --------------------------------------------
            Binding::new(K::Up, none, A::Up, C::List, "k/↑"),
            Binding::new(K::Char('k'), none, A::Up, C::List, "k/↑"),
            Binding::new(K::Down, none, A::Down, C::List, "j/↓"),
            Binding::new(K::Char('j'), none, A::Down, C::List, "j/↓"),
            Binding::new(K::PageUp, none, A::PageUp, C::List, "PgUp"),
            Binding::new(K::PageDown, none, A::PageDown, C::List, "PgDn"),
            Binding::new(K::Char('u'), ctrl, A::PageUp, C::List, "C-u"),
            Binding::new(K::Char('d'), ctrl, A::PageDown, C::List, "C-d"),
            Binding::new(K::Home, none, A::Top, C::List, "Home"),
            Binding::new(K::Char('g'), none, A::Top, C::List, "g"),
            Binding::new(K::End, none, A::Bottom, C::List, "End"),
            Binding::new(K::Char('G'), none, A::Bottom, C::List, "G"),
            Binding::new(K::Char('/'), none, A::Search, C::List, "/"),
            Binding::new(K::Char('n'), none, A::SearchNext, C::List, "n"),
            // -- home -----------------------------------------------------
            Binding::new(K::Char('s'), none, A::StartCore, C::Home, "s"),
            Binding::new(K::Char('S'), none, A::StopCore, C::Home, "S"),
            Binding::new(K::Char('R'), none, A::RestartCore, C::Home, "R"),
            Binding::new(K::Char('M'), none, A::CycleCoreMode, C::Home, "M"),
            Binding::new(K::Char('U'), none, A::UpgradeCore, C::Home, "U"),
            Binding::new(K::Char('g'), none, A::UpdateGeo, C::Home, "g"),
            Binding::new(K::Char('F'), none, A::FlushCaches, C::Home, "F"),
            Binding::new(K::Char('e'), none, A::EditRuntimeConfig, C::Home, "e"),
            // -- profiles --------------------------------------------------
            Binding::new(K::Enter, none, A::ActivateProfile, C::Profiles, "Enter"),
            Binding::new(K::Char('u'), none, A::UpdateProfile, C::Profiles, "u"),
            Binding::new(K::Char('U'), none, A::UpdateAllProfiles, C::Profiles, "U"),
            Binding::new(K::Char('a'), none, A::NewProfile, C::Profiles, "a"),
            Binding::new(K::Char('d'), none, A::DeleteProfile, C::Profiles, "d"),
            Binding::new(K::Char('R'), none, A::RenameProfile, C::Profiles, "R"),
            Binding::new(K::Char('i'), none, A::ImportProfiles, C::Profiles, "i"),
            Binding::new(K::Char('e'), none, A::EditProfile, C::Profiles, "e"),
            Binding::new(K::Char('c'), none, A::ToggleInChain, C::Profiles, "c"),
            Binding::new(K::Char('p'), none, A::PreviewConfig, C::Profiles, "p"),
            Binding::new(K::Char('A'), none, A::ApplyConfig, C::Profiles, "A"),
            Binding::new(K::Char('b'), none, A::RollbackConfig, C::Profiles, "b"),
            // -- proxies ---------------------------------------------------
            Binding::new(K::Enter, none, A::SelectNode, C::Proxies, "Enter"),
            Binding::new(K::Char('t'), none, A::TestNode, C::Proxies, "t"),
            Binding::new(K::Char('T'), none, A::TestGroup, C::Proxies, "T"),
            Binding::new(K::Char('a'), none, A::TestAllNodes, C::Proxies, "a"),
            Binding::new(K::Char('b'), none, A::TestRouteSpeed, C::Proxies, "b"),
            Binding::new(K::Char('B'), none, A::InstallSpeedtestGo, C::Proxies, "B"),
            Binding::new(K::Char('v'), none, A::CycleTestMode, C::Proxies, "v"),
            Binding::new(K::Char('s'), none, A::CycleNodeSort, C::Proxies, "s"),
            Binding::new(K::Char('x'), none, A::ClearNodeSelection, C::Proxies, "x"),
            Binding::new(K::Char('M'), none, A::CycleCoreMode, C::Proxies, "M"),
            // -- connections -----------------------------------------------
            Binding::new(K::Char('d'), none, A::CloseConnection, C::Connections, "d"),
            Binding::new(
                K::Char('D'),
                none,
                A::CloseAllConnections,
                C::Connections,
                "D",
            ),
            Binding::new(
                K::Char('s'),
                none,
                A::CycleConnectionSort,
                C::Connections,
                "s",
            ),
            Binding::new(K::Char('c'), none, A::CloseConnection, C::Connections, "c"),
            // -- logs ------------------------------------------------------
            Binding::new(K::Char('f'), none, A::ToggleLogFollow, C::Logs, "f"),
            Binding::new(K::Up, none, A::Up, C::Logs, "k/↑"),
            Binding::new(K::Char('k'), none, A::Up, C::Logs, "k/↑"),
            Binding::new(K::Down, none, A::Down, C::Logs, "j/↓"),
            Binding::new(K::Char('j'), none, A::Down, C::Logs, "j/↓"),
            Binding::new(K::PageUp, none, A::PageUp, C::Logs, "PgUp"),
            Binding::new(K::PageDown, none, A::PageDown, C::Logs, "PgDn"),
            Binding::new(K::Home, none, A::Top, C::Logs, "Home"),
            Binding::new(K::End, none, A::Bottom, C::Logs, "End"),
            Binding::new(K::Char('l'), none, A::CycleLogLevel, C::Logs, "l"),
            Binding::new(K::Char('c'), none, A::ClearLogs, C::Logs, "c"),
            Binding::new(K::Char('x'), none, A::ExportLogs, C::Logs, "x"),
            // -- rules -----------------------------------------------------
            Binding::new(K::Enter, none, A::ToggleRule, C::Rules, "Enter"),
            Binding::new(K::Char(' '), none, A::ToggleRule, C::Rules, "Space"),
            Binding::new(K::Char('u'), none, A::UpdateRuleProvider, C::Rules, "u"),
            Binding::new(K::Char('U'), none, A::UpdateAllRuleProviders, C::Rules, "U"),
            Binding::new(K::Char('h'), none, A::ToggleDisabledRules, C::Rules, "h"),
            Binding::new(K::Char('M'), none, A::CycleCoreMode, C::Rules, "M"),
            // -- tests -----------------------------------------------------
            Binding::new(K::Enter, none, A::RunTests, C::Tests, "Enter"),
            Binding::new(K::Char('a'), none, A::RunAllTests, C::Tests, "a"),
            Binding::new(K::Char('s'), none, A::CancelTests, C::Tests, "s"),
            Binding::new(K::Char('c'), none, A::ClearTestResults, C::Tests, "c"),
            // -- settings --------------------------------------------------
            Binding::new(K::Enter, none, A::ToggleSetting, C::Settings, "Enter"),
            Binding::new(K::Char(' '), none, A::ToggleSetting, C::Settings, "Space"),
            Binding::new(K::Char('s'), none, A::SaveSettings, C::Settings, "s"),
        ];

        // Number keys jump straight to a tab.
        for (index, screen) in Screen::all().into_iter().enumerate() {
            let digit = char::from(b'1' + u8::try_from(index).unwrap_or(0));
            let display = match index {
                0 => "1-9",
                _ => "",
            };
            bindings.push(Binding::new(
                K::Char(digit),
                none,
                A::Goto(screen),
                C::Global,
                display,
            ));
        }

        Self { bindings }
    }

    /// Every binding.
    #[must_use]
    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    /// The action bound to a key press on a screen, if any.
    ///
    /// The screen's most specific context is consulted first, so a
    /// screen-specific binding always beats a global one.
    #[must_use]
    pub fn resolve(&self, screen: Screen, event: KeyEvent) -> Option<Action> {
        let contexts = contexts_for(screen);
        for context in contexts {
            if let Some(binding) = self
                .bindings
                .iter()
                .find(|b| b.context == context && b.matches(event))
            {
                return Some(binding.action.clone());
            }
        }
        None
    }

    /// The bindings that apply on a screen, most specific first.
    #[must_use]
    pub fn for_screen(&self, screen: Screen) -> Vec<&Binding> {
        let contexts = contexts_for(screen);
        let mut out: Vec<&Binding> = Vec::new();
        for context in contexts {
            out.extend(self.bindings.iter().filter(|b| b.context == context));
        }
        out
    }

    /// The hint pairs shown in the footer for a screen, capped at `limit`.
    ///
    /// Screen-specific bindings are listed before global ones, because those
    /// are the ones the user cannot guess.
    ///
    /// Hints are keyed by *action*, not by binding: an action with two keys
    /// (a primary and an alias) appears once, with its primary key. Listing it
    /// twice would spend footer space saying nothing new.
    #[must_use]
    pub fn hints(&self, screen: Screen, limit: usize) -> Vec<(&'static str, &'static str)> {
        self.hint_bindings(screen, limit)
            .into_iter()
            .map(|(key, action)| (key, action.label()))
            .collect()
    }

    /// The footer hints with action identity preserved for localization.
    #[must_use]
    pub fn hint_bindings(&self, screen: Screen, limit: usize) -> Vec<(&'static str, &Action)> {
        let mut out: Vec<(&'static str, &Action)> = Vec::new();
        let mut seen_actions: Vec<&Action> = Vec::new();
        for binding in self.for_screen(screen) {
            // The digit shortcuts collapse into a single "1-9" hint.
            if binding.display.is_empty() || seen_actions.contains(&&binding.action) {
                continue;
            }
            seen_actions.push(&binding.action);
            let hint = (binding.display, &binding.action);
            // Also dedupe by rendered text: two distinct actions that read the
            // same would waste a footer slot saying nothing new.
            if out
                .iter()
                .any(|(key, action)| *key == hint.0 && action.label() == hint.1.label())
            {
                continue;
            }
            out.push(hint);
            if out.len() >= limit {
                break;
            }
        }
        out
    }

    /// Find the display string for an action on a screen, for the help screen.
    #[must_use]
    pub fn keys_for(&self, action: &Action) -> Vec<&'static str> {
        let mut keys: Vec<&'static str> = self
            .bindings
            .iter()
            .filter(|b| &b.action == action && !b.display.is_empty())
            .map(|b| b.display)
            .collect();
        if let Action::Goto(screen) = action {
            // The footer groups all nine jumps into `1-9`, while the help
            // screen names the exact key for each destination.
            keys.retain(|key| *key != "1-9");
            const DIGITS: &[&str] = &["1", "2", "3", "4", "5", "6", "7", "8", "9"];
            if let Some(index) = Screen::all()
                .iter()
                .position(|candidate| candidate == screen)
            {
                keys.push(DIGITS[index]);
            }
        }
        keys.dedup();
        keys
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    /// Every action in the enum is reachable from a key, and every key names an
    /// action that exists.
    ///
    /// Textual, like the diagnostic-code scan in `cvt-core`, and for the same
    /// reason: the two lists — the enum and the keymap — have to agree, nothing
    /// in the type system makes them, and a key that does nothing is the
    /// interface's version of a check that covers the members somebody named.
    ///
    /// The dispatch side is asserted in `app.rs`, where the handlers are.
    #[test]
    fn every_action_is_reachable_from_a_key() {
        let actions = include_str!("action.rs");
        let defined: Vec<&str> = actions
            .split("pub enum Action {")
            .nth(1)
            .unwrap_or_default()
            .split("\n}")
            .next()
            .unwrap_or_default()
            .lines()
            .filter_map(|line| {
                let name = line
                    .trim()
                    .split(['(', '{', ','])
                    .next()
                    .unwrap_or_default()
                    .trim();
                (line.starts_with("    ")
                    && !line.starts_with("     ")
                    && name.chars().next().is_some_and(char::is_uppercase))
                .then_some(name)
            })
            .collect();
        assert!(
            defined.len() > 40,
            "the scan found only {} actions, so it is looking in the wrong place",
            defined.len()
        );

        let bound: Vec<&str> = include_str!("keys.rs")
            .match_indices("Action::")
            .filter_map(|(at, _)| {
                let rest = &include_str!("keys.rs")[at + "Action::".len()..];
                let name: String = rest
                    .chars()
                    .take_while(char::is_ascii_alphanumeric)
                    .collect();
                (!name.is_empty()).then_some(name)
            })
            .map(|name| Box::leak(name.into_boxed_str()) as &str)
            .collect();

        let unreachable: Vec<&&str> = defined
            .iter()
            .filter(|name| !bound.contains(*name))
            .collect();
        assert!(
            unreachable.is_empty(),
            "these actions are defined and no key reaches them: {unreachable:?}"
        );

        let unknown: Vec<&&str> = bound
            .iter()
            .filter(|name| !defined.contains(*name))
            .collect();
        assert!(
            unknown.is_empty(),
            "the keymap names actions that do not exist: {unknown:?}"
        );
    }
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn no_key_is_bound_twice_within_one_context() {
        let map = Keymap::new();
        let mut seen: Vec<(Context, KeyCode, KeyModifiers)> = Vec::new();
        for b in map.bindings() {
            let relevant = b.mods & (KeyModifiers::CONTROL | KeyModifiers::ALT);
            let entry = (b.context, b.key, relevant);
            assert!(
                !seen.contains(&entry),
                "`{}` is bound twice in {:?}",
                b.display,
                b.context
            );
            seen.push(entry);
        }
    }

    #[test]
    fn every_action_is_reachable_from_at_least_one_screen() {
        let map = Keymap::new();
        for action in [
            Action::Quit,
            Action::Refresh,
            Action::Cancel,
            Action::Search,
            Action::SearchNext,
            Action::Up,
            Action::Down,
            Action::PageUp,
            Action::PageDown,
            Action::Top,
            Action::Bottom,
            Action::NextScreen,
            Action::PreviousScreen,
            Action::ShowLastMessage,
            Action::StartCore,
            Action::StopCore,
            Action::RestartCore,
            Action::CycleCoreMode,
            Action::UpgradeCore,
            Action::UpdateGeo,
            Action::FlushCaches,
            Action::EditRuntimeConfig,
            Action::ActivateProfile,
            Action::UpdateProfile,
            Action::UpdateAllProfiles,
            Action::NewProfile,
            Action::DeleteProfile,
            Action::RenameProfile,
            Action::ImportProfiles,
            Action::EditProfile,
            Action::ToggleInChain,
            Action::PreviewConfig,
            Action::ApplyConfig,
            Action::RollbackConfig,
            Action::SelectNode,
            Action::TestNode,
            Action::TestGroup,
            Action::TestAllNodes,
            Action::TestRouteSpeed,
            Action::InstallSpeedtestGo,
            Action::CycleTestMode,
            Action::ClearNodeSelection,
            Action::CloseConnection,
            Action::CloseAllConnections,
            Action::CycleConnectionSort,
            Action::CycleNodeSort,
            Action::ToggleLogFollow,
            Action::CycleLogLevel,
            Action::ClearLogs,
            Action::ExportLogs,
            Action::ToggleRule,
            Action::UpdateRuleProvider,
            Action::UpdateAllRuleProviders,
            Action::ToggleDisabledRules,
            Action::RunTests,
            Action::RunAllTests,
            Action::CancelTests,
            Action::ClearTestResults,
            Action::SaveSettings,
            Action::ToggleSetting,
        ] {
            let reachable = Screen::all().into_iter().any(|s| {
                map.bindings()
                    .iter()
                    .any(|b| b.action == action && contexts_for(s).contains(&b.context))
            });
            assert!(reachable, "{action:?} cannot be triggered from any screen");
        }
        for screen in Screen::all() {
            assert_eq!(
                map.resolve(screen, key(KeyCode::Char('1'))),
                Some(Action::Goto(Screen::Home))
            );
        }
    }

    #[test]
    fn a_screen_specific_binding_beats_a_global_one() {
        let map = Keymap::new();
        // `s` starts the core on the dashboard...
        assert_eq!(
            map.resolve(Screen::Home, key(KeyCode::Char('s'))),
            Some(Action::StartCore)
        );
        // ...but saves the settings on the settings screen.
        assert_eq!(
            map.resolve(Screen::Settings, key(KeyCode::Char('s'))),
            Some(Action::SaveSettings)
        );
        // ...and does nothing on the logs screen, where it is unbound.
        assert_eq!(map.resolve(Screen::Logs, key(KeyCode::Char('s'))), None);
    }

    #[test]
    fn the_list_context_applies_wherever_there_is_a_list_but_not_elsewhere() {
        let map = Keymap::new();
        for screen in [
            Screen::Profiles,
            Screen::Proxies,
            Screen::Connections,
            Screen::Rules,
        ] {
            assert_eq!(
                map.resolve(screen, key(KeyCode::Down)),
                Some(Action::Down),
                "{screen}"
            );
        }
        assert_eq!(
            map.resolve(Screen::Logs, key(KeyCode::Down)),
            Some(Action::Down)
        );
        assert_eq!(map.resolve(Screen::Home, key(KeyCode::Down)), None);
    }

    #[test]
    fn global_bindings_work_everywhere() {
        let map = Keymap::new();
        for screen in Screen::all() {
            assert_eq!(
                map.resolve(screen, ctrl('c')),
                Some(Action::Quit),
                "{screen}"
            );
            assert_eq!(
                map.resolve(screen, key(KeyCode::Tab)),
                Some(Action::NextScreen),
                "{screen}"
            );
            assert_eq!(
                map.resolve(screen, key(KeyCode::Esc)),
                Some(Action::Cancel),
                "{screen}"
            );
            assert_eq!(
                map.resolve(screen, key(KeyCode::Char('r'))),
                Some(Action::Refresh),
                "{screen}"
            );
            assert_eq!(
                map.resolve(screen, key(KeyCode::Char('?'))),
                Some(Action::Goto(Screen::Help)),
                "{screen}"
            );
        }
    }

    #[test]
    fn digits_jump_to_every_tab() {
        let map = Keymap::new();
        for (index, screen) in Screen::all().into_iter().enumerate() {
            let digit = char::from(b'1' + u8::try_from(index).unwrap());
            assert_eq!(
                map.resolve(Screen::Profiles, key(KeyCode::Char(digit))),
                Some(Action::Goto(screen)),
                "digit {digit} should open {screen}"
            );
        }
        assert_eq!(map.resolve(Screen::Profiles, key(KeyCode::Char('0'))), None);
    }

    #[test]
    fn an_uppercase_letter_is_not_treated_as_shift_modified() {
        // Terminals report SHIFT for a capital, which must not make `U`
        // unreachable because it was bound without a modifier.
        let map = Keymap::new();
        let shifted = KeyEvent::new(KeyCode::Char('U'), KeyModifiers::SHIFT);
        assert_eq!(
            map.resolve(Screen::Profiles, shifted),
            Some(Action::UpdateAllProfiles),
            "SHIFT must be ignored so differing terminal behaviour does not matter"
        );
        // But a real modifier still distinguishes bindings.
        let with_ctrl = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(map.resolve(Screen::Home, with_ctrl), Some(Action::Quit));
    }

    #[test]
    fn a_significant_modifier_prevents_a_match() {
        let map = Keymap::new();
        let alt_u = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::ALT);
        assert_eq!(
            map.resolve(Screen::Profiles, alt_u),
            None,
            "ALT+u is a different key from u"
        );
    }

    #[test]
    fn an_unbound_key_resolves_to_nothing() {
        let map = Keymap::new();
        assert_eq!(map.resolve(Screen::Home, key(KeyCode::F(9))), None);
        assert_eq!(map.resolve(Screen::Logs, key(KeyCode::Char('z'))), None);
    }

    #[test]
    fn footer_hints_are_short_unique_and_lead_with_screen_bindings() {
        let map = Keymap::new();
        for screen in Screen::all() {
            let hints = map.hints(screen, 8);
            assert!(!hints.is_empty(), "{screen} has no footer hints");
            assert!(hints.len() <= 8);
            let mut labels: Vec<&str> = hints.iter().map(|(_, l)| *l).collect();
            let count = labels.len();
            labels.sort_unstable();
            labels.dedup();
            assert_eq!(labels.len(), count, "{screen} repeats a hint on one line");
            // An action reachable by two keys must not consume two slots.
            assert!(
                hints.iter().filter(|(_, l)| *l == "close").count() <= 1,
                "an aliased action must appear once: {hints:?}"
            );
            for (key_label, label) in &hints {
                assert!(!key_label.is_empty() && !label.is_empty());
            }
        }
        // The digit shortcut collapses rather than filling the whole footer.
        assert!(map.hints(Screen::Home, 20).iter().any(|(k, _)| *k == "1-9"));
    }

    #[test]
    fn the_help_screen_can_look_up_the_keys_for_an_action() {
        let map = Keymap::new();
        assert!(map.keys_for(&Action::Quit).contains(&"q"));
        assert!(map.keys_for(&Action::ToggleRule).contains(&"Enter"));
        assert!(map.keys_for(&Action::ToggleRule).contains(&"Space"));
        assert!(map.keys_for(&Action::StartCore).contains(&"s"));
    }

    #[test]
    fn every_binding_documents_itself() {
        let map = Keymap::new();
        for binding in map.bindings() {
            assert!(!binding.action.label().is_empty());
            assert!(!binding.action.help().is_empty());
        }
    }
}
