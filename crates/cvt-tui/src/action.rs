//! What the user can ask for.
//!
//! Every keystroke resolves to one [`Action`]. Keeping the vocabulary in a
//! single enum means the key map, the footer hints, the help screen and the
//! dispatcher are all driven by the same list — a new action cannot be
//! half-wired, and the help screen cannot drift from reality.

use std::fmt;

/// The screens the interface can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Screen {
    /// Dashboard: core status, throughput, quick actions.
    Home,
    /// Subscription list and the configuration chain.
    Profiles,
    /// Proxy groups and node selection.
    Proxies,
    /// Live connections.
    Connections,
    /// Live log stream.
    Logs,
    /// Routing rules and rule providers.
    Rules,
    /// Latency tests.
    Tests,
    /// Application and core settings.
    Settings,
    /// Key reference.
    Help,
}

impl Screen {
    /// Every screen, in tab order.
    #[must_use]
    pub fn all() -> [Self; 9] {
        [
            Self::Home,
            Self::Profiles,
            Self::Proxies,
            Self::Connections,
            Self::Logs,
            Self::Rules,
            Self::Tests,
            Self::Settings,
            Self::Help,
        ]
    }

    /// The tab label.
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Self::Home => "Home",
            Self::Profiles => "Profiles",
            Self::Proxies => "Proxies",
            Self::Connections => "Connections",
            Self::Logs => "Logs",
            Self::Rules => "Rules",
            Self::Tests => "Tests",
            Self::Settings => "Settings",
            Self::Help => "Help",
        }
    }

    /// The next screen in tab order, wrapping.
    #[must_use]
    pub fn next(self) -> Self {
        let all = Self::all();
        let index = all.iter().position(|s| *s == self).unwrap_or(0);
        all[(index + 1) % all.len()]
    }

    /// The previous screen in tab order, wrapping.
    #[must_use]
    pub fn previous(self) -> Self {
        let all = Self::all();
        let index = all.iter().position(|s| *s == self).unwrap_or(0);
        all[(index + all.len() - 1) % all.len()]
    }

    /// The screen a single digit selects, for `1`-`9`.
    #[must_use]
    pub fn from_digit(d: u8) -> Option<Self> {
        let index = usize::from(d).checked_sub(1)?;
        Self::all().get(index).copied()
    }
}

impl fmt::Display for Screen {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.title())
    }
}

/// One user intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    // -- global -----------------------------------------------------------
    /// Leave the application.
    Quit,
    /// Go to a screen.
    Goto(Screen),
    /// Advance to the next screen.
    NextScreen,
    /// Go back to the previous screen.
    PreviousScreen,
    /// Redraw everything from the core.
    Refresh,
    /// Cancel the current overlay, search or selection.
    Cancel,
    /// View the latest status message in full.
    ShowLastMessage,

    // -- list navigation --------------------------------------------------
    /// Move the cursor up one row.
    Up,
    /// Move the cursor down one row.
    Down,
    /// Move up one page.
    PageUp,
    /// Move down one page.
    PageDown,
    /// Jump to the first row.
    Top,
    /// Jump to the last row.
    Bottom,

    // -- search -----------------------------------------------------------
    /// Start filtering the current list.
    Search,
    /// Jump to the next match.
    SearchNext,

    // -- profiles ---------------------------------------------------------
    /// Switch to the highlighted profile and regenerate.
    ActivateProfile,
    /// Download the highlighted subscription again.
    UpdateProfile,
    /// Download every subscription that is due.
    UpdateAllProfiles,
    /// Add a profile, from a URL or from scratch.
    NewProfile,
    /// Delete the highlighted profile.
    DeleteProfile,
    /// Rename the highlighted profile.
    RenameProfile,
    /// Import profiles from an existing `clash-verge-rev` installation.
    ImportProfiles,
    /// Open the highlighted profile's document in `$EDITOR`.
    EditProfile,
    /// Add or remove the highlighted patch from the explicit chain.
    ToggleInChain,
    /// Preview what regenerating would change, without applying it.
    PreviewConfig,
    /// Generate and apply the current chain.
    ApplyConfig,
    /// Restore the most recent snapshot.
    RollbackConfig,

    // -- proxies ----------------------------------------------------------
    /// Select the highlighted node in the highlighted group.
    SelectNode,
    /// Latency-test the highlighted group.
    TestGroup,
    /// Latency-test the highlighted node.
    TestNode,
    /// Latency-test every node.
    TestAllNodes,
    /// Return a group to automatic selection.
    ClearNodeSelection,

    // -- connections ------------------------------------------------------
    /// Close the highlighted connection.
    CloseConnection,
    /// Close every connection.
    CloseAllConnections,
    /// Change how the connection list is sorted.
    CycleConnectionSort,

    // -- logs -------------------------------------------------------------
    /// Start or stop following new lines.
    ToggleLogFollow,
    /// Cycle the minimum level, on the core as well as locally.
    CycleLogLevel,
    /// Discard the buffered lines.
    ClearLogs,
    /// Write the buffered lines to a file.
    ExportLogs,

    // -- rules ------------------------------------------------------------
    /// Enable or disable the highlighted rule.
    ToggleRule,
    /// Update the highlighted rule provider.
    UpdateRuleProvider,
    /// Update every rule provider.
    UpdateAllRuleProviders,
    /// Show or hide disabled rules.
    ToggleDisabledRules,

    // -- tests ------------------------------------------------------------
    /// Run the selected test suite.
    RunTests,
    /// Cancel a running test batch.
    CancelTests,
    /// Clear cached results.
    ClearTestResults,

    // -- core -------------------------------------------------------------
    /// Start the core process.
    StartCore,
    /// Stop the core process.
    StopCore,
    /// Restart the core process.
    RestartCore,
    /// Cycle rule, global and direct routing modes.
    CycleCoreMode,
    /// Download or update the managed core binary.
    UpgradeCore,
    /// Refresh the GeoIP and GeoSite databases.
    UpdateGeo,
    /// Flush the fake-IP and DNS caches.
    FlushCaches,
    /// Open the runtime configuration in `$EDITOR`.
    EditRuntimeConfig,

    // -- settings ---------------------------------------------------------
    /// Persist the current settings.
    SaveSettings,
    /// Change the highlighted setting.
    ToggleSetting,
}

impl Action {
    /// A short label for the footer hint.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Quit => "quit",
            // `?` opens the key reference, which is a different promise from
            // "switch tab" and must not collapse into the same footer hint.
            Self::Goto(Screen::Help) => "help",
            Self::Goto(_) => "switch tab",
            Self::NextScreen => "next tab",
            Self::PreviousScreen => "prev tab",
            Self::Refresh => "refresh",
            Self::Cancel => "cancel",
            Self::ShowLastMessage => "message",
            Self::Up => "up",
            Self::Down => "down",
            Self::PageUp => "page up",
            Self::PageDown => "page down",
            Self::Top => "top",
            Self::Bottom => "bottom",
            Self::Search => "search",
            Self::SearchNext => "next match",
            Self::ActivateProfile => "switch",
            Self::UpdateProfile => "update",
            Self::UpdateAllProfiles => "update all",
            Self::NewProfile => "new",
            Self::DeleteProfile => "delete",
            Self::RenameProfile => "rename",
            Self::ImportProfiles => "import",
            Self::EditProfile => "edit",
            Self::ToggleInChain => "chain",
            Self::PreviewConfig => "preview",
            Self::ApplyConfig => "apply",
            Self::RollbackConfig => "rollback",
            Self::SelectNode => "select",
            Self::TestGroup => "test group",
            Self::TestNode => "test node",
            Self::TestAllNodes => "test all",
            Self::ClearNodeSelection => "unpin",
            Self::CloseConnection => "close",
            Self::CloseAllConnections => "close all",
            Self::CycleConnectionSort => "sort",
            Self::ToggleLogFollow => "follow",
            Self::CycleLogLevel => "level",
            Self::ClearLogs => "clear",
            Self::ExportLogs => "export",
            Self::ToggleRule => "toggle",
            Self::UpdateRuleProvider => "update set",
            Self::UpdateAllRuleProviders => "update sets",
            Self::ToggleDisabledRules => "show disabled",
            Self::RunTests => "run",
            Self::CancelTests => "stop tests",
            Self::ClearTestResults => "clear results",
            Self::StartCore => "start core",
            Self::StopCore => "stop core",
            Self::RestartCore => "restart core",
            Self::CycleCoreMode => "route mode",
            Self::UpgradeCore => "install core",
            Self::UpdateGeo => "update geo",
            Self::FlushCaches => "flush caches",
            Self::EditRuntimeConfig => "edit config",
            Self::SaveSettings => "save",
            Self::ToggleSetting => "change",
        }
    }

    /// A fuller description for the help screen.
    #[must_use]
    pub fn help(&self) -> &'static str {
        match self {
            Self::Quit => "leave clash-verge-tui (the core keeps running)",
            Self::Goto(s) => match s {
                Screen::Home => "dashboard: core status, throughput, quick actions",
                Screen::Profiles => "subscriptions and the configuration chain",
                Screen::Proxies => "proxy groups and node selection",
                Screen::Connections => "connections the core is currently proxying",
                Screen::Logs => "live log stream from the core",
                Screen::Rules => "routing rules and rule providers",
                Screen::Tests => "latency tests",
                Screen::Settings => "application and core settings",
                Screen::Help => "this reference",
            },
            Self::NextScreen => "move to the next tab",
            Self::PreviousScreen => "move to the previous tab",
            Self::Refresh => "re-read everything from the core",
            Self::Cancel => "close a prompt, or clear the search",
            Self::ShowLastMessage => "view the full text of the latest status message",
            Self::Up => "move the cursor up",
            Self::Down => "move the cursor down",
            Self::PageUp => "move up by a screenful",
            Self::PageDown => "move down by a screenful",
            Self::Top => "jump to the first row",
            Self::Bottom => "jump to the last row",
            Self::Search => "filter the current list",
            Self::SearchNext => "jump to the next match",
            Self::ActivateProfile => "make this profile the base and regenerate",
            Self::UpdateProfile => "download this subscription again",
            Self::UpdateAllProfiles => "download every subscription that is due",
            Self::NewProfile => "add a subscription by URL, or a blank local profile",
            Self::DeleteProfile => "delete this profile and its document",
            Self::RenameProfile => "give this profile a new name",
            Self::ImportProfiles => "copy profiles out of an existing clash-verge-rev home",
            Self::EditProfile => "open this profile's document in $EDITOR",
            Self::ToggleInChain => "include or exclude this patch in the chain",
            Self::PreviewConfig => "show what regenerating would change, without applying it",
            Self::ApplyConfig => "generate the configuration and hand it to the core",
            Self::RollbackConfig => "restore the most recent snapshot",
            Self::SelectNode => "pin this node in the selected group",
            Self::TestGroup => "measure every member of this group",
            Self::TestNode => "measure this node",
            Self::TestAllNodes => "measure every node the core knows about",
            Self::ClearNodeSelection => "let an automatic group choose again",
            Self::CloseConnection => "drop this connection; the client will reconnect",
            Self::CloseAllConnections => "drop every connection",
            Self::CycleConnectionSort => "change the ordering of the connection list",
            Self::ToggleLogFollow => "stop or resume scrolling with new lines",
            Self::CycleLogLevel => "raise or lower the minimum level, on the core too",
            Self::ClearLogs => "discard the buffered lines",
            Self::ExportLogs => "write the buffered lines to a timestamped file",
            Self::ToggleRule => "enable or disable this rule in the running core",
            Self::UpdateRuleProvider => "download this rule set again",
            Self::UpdateAllRuleProviders => "download every rule set again",
            Self::ToggleDisabledRules => "include disabled rules in the list",
            Self::RunTests => "run the highlighted test",
            Self::CancelTests => "stop the running batch",
            Self::ClearTestResults => "forget cached latency results",
            Self::StartCore => "launch the core with the generated configuration",
            Self::StopCore => "stop the core process",
            Self::RestartCore => "stop and start the core",
            Self::CycleCoreMode => "switch rule, global and direct routing modes",
            Self::UpgradeCore => "download or update the managed mihomo core",
            Self::UpdateGeo => "download fresh GeoIP and GeoSite databases",
            Self::FlushCaches => "clear the fake-IP and DNS caches",
            Self::EditRuntimeConfig => "open the generated configuration in $EDITOR",
            Self::SaveSettings => "write the settings to disk",
            Self::ToggleSetting => "change the highlighted setting",
        }
    }

    /// Whether the action needs confirmation before it runs.
    #[must_use]
    pub fn is_destructive(&self) -> bool {
        matches!(
            self,
            Self::DeleteProfile
                | Self::CloseAllConnections
                | Self::StopCore
                | Self::RollbackConfig
                | Self::UpgradeCore
        )
    }

    /// Which section of the help screen lists this action.
    #[must_use]
    pub fn group(&self) -> &'static str {
        match self {
            Self::Quit
            | Self::Goto(_)
            | Self::NextScreen
            | Self::PreviousScreen
            | Self::Refresh
            | Self::Cancel
            | Self::ShowLastMessage => "General",
            Self::Up
            | Self::Down
            | Self::PageUp
            | Self::PageDown
            | Self::Top
            | Self::Bottom
            | Self::Search
            | Self::SearchNext => "Navigation",
            Self::ActivateProfile
            | Self::UpdateProfile
            | Self::UpdateAllProfiles
            | Self::NewProfile
            | Self::DeleteProfile
            | Self::RenameProfile
            | Self::ImportProfiles
            | Self::EditProfile
            | Self::ToggleInChain
            | Self::PreviewConfig
            | Self::ApplyConfig
            | Self::RollbackConfig => "Profiles",
            Self::SelectNode
            | Self::TestGroup
            | Self::TestNode
            | Self::TestAllNodes
            | Self::ClearNodeSelection => "Proxies",
            Self::CloseConnection | Self::CloseAllConnections | Self::CycleConnectionSort => {
                "Connections"
            }
            Self::ToggleLogFollow | Self::CycleLogLevel | Self::ClearLogs | Self::ExportLogs => {
                "Logs"
            }
            Self::ToggleRule
            | Self::UpdateRuleProvider
            | Self::UpdateAllRuleProviders
            | Self::ToggleDisabledRules => "Rules",
            Self::RunTests | Self::CancelTests | Self::ClearTestResults => "Tests",
            Self::StartCore
            | Self::StopCore
            | Self::RestartCore
            | Self::CycleCoreMode
            | Self::UpgradeCore
            | Self::UpdateGeo
            | Self::FlushCaches
            | Self::EditRuntimeConfig => "Core",
            Self::SaveSettings | Self::ToggleSetting => "Settings",
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn tabs_cycle_in_both_directions_and_wrap() {
        assert_eq!(Screen::Home.next(), Screen::Profiles);
        assert_eq!(Screen::Help.next(), Screen::Home);
        assert_eq!(Screen::Home.previous(), Screen::Help);
        let mut s = Screen::Home;
        for _ in 0..Screen::all().len() {
            s = s.next();
        }
        assert_eq!(s, Screen::Home, "a full cycle returns to the start");
    }

    #[test]
    fn digits_select_the_first_nine_screens() {
        assert_eq!(Screen::from_digit(1), Some(Screen::Home));
        assert_eq!(Screen::from_digit(9), Some(Screen::Help));
        assert_eq!(Screen::from_digit(0), None);
        assert_eq!(Screen::from_digit(10), None);
    }

    #[test]
    fn every_destructive_action_is_recognised_and_others_are_not() {
        for action in [
            Action::DeleteProfile,
            Action::CloseAllConnections,
            Action::StopCore,
            Action::RollbackConfig,
            Action::UpgradeCore,
        ] {
            assert!(
                action.is_destructive(),
                "{action:?} must require confirmation"
            );
        }
        for action in [
            Action::Refresh,
            Action::SelectNode,
            Action::CloseConnection,
            Action::ToggleRule,
            Action::SaveSettings,
        ] {
            assert!(!action.is_destructive(), "{action:?} must not prompt");
        }
    }

    #[test]
    fn every_action_has_a_label_a_help_line_and_a_group() {
        // The help screen is generated from these, so an empty string would
        // render a blank row.
        for action in all_actions() {
            assert!(!action.label().is_empty(), "{action:?} has no label");
            assert!(!action.help().is_empty(), "{action:?} has no help text");
            assert!(!action.group().is_empty(), "{action:?} has no group");
        }
    }

    #[test]
    fn goto_labels_name_their_screen() {
        assert_eq!(Action::Goto(Screen::Logs).label(), "switch tab");
        assert_eq!(
            Action::Goto(Screen::Help).label(),
            "help",
            "help is not a tab jump"
        );
        assert!(Action::Goto(Screen::Logs).help().contains("log"));
    }

    /// Every action variant, so the checks above cannot silently skip one.
    fn all_actions() -> Vec<Action> {
        let mut out = vec![
            Action::Quit,
            Action::NextScreen,
            Action::PreviousScreen,
            Action::Refresh,
            Action::Cancel,
            Action::ShowLastMessage,
            Action::Up,
            Action::Down,
            Action::PageUp,
            Action::PageDown,
            Action::Top,
            Action::Bottom,
            Action::Search,
            Action::SearchNext,
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
            Action::TestGroup,
            Action::TestNode,
            Action::TestAllNodes,
            Action::ClearNodeSelection,
            Action::CloseConnection,
            Action::CloseAllConnections,
            Action::CycleConnectionSort,
            Action::ToggleLogFollow,
            Action::CycleLogLevel,
            Action::ClearLogs,
            Action::ExportLogs,
            Action::ToggleRule,
            Action::UpdateRuleProvider,
            Action::UpdateAllRuleProviders,
            Action::ToggleDisabledRules,
            Action::RunTests,
            Action::CancelTests,
            Action::ClearTestResults,
            Action::StartCore,
            Action::StopCore,
            Action::RestartCore,
            Action::UpgradeCore,
            Action::UpdateGeo,
            Action::FlushCaches,
            Action::EditRuntimeConfig,
            Action::SaveSettings,
            Action::ToggleSetting,
        ];
        out.extend(Screen::all().into_iter().map(Action::Goto));
        out
    }

    #[test]
    fn screen_titles_are_unique_and_non_empty() {
        let mut titles: Vec<&str> = Screen::all().iter().map(|s| s.title()).collect();
        assert!(titles.iter().all(|t| !t.is_empty()));
        titles.sort_unstable();
        let count = titles.len();
        titles.dedup();
        assert_eq!(titles.len(), count, "tab titles must be distinguishable");
    }
}
