use super::*;

// -- navigation ---------------------------------------------------------

#[test]
fn movement_clamps_and_pages_by_a_screenful() {
    let mut a = loaded();
    goto(&mut a, Screen::Proxies);
    a.expanded.push("PROXY".to_owned());
    let _ = a.on_event(Event::Data(Data::Nodes(vec![
        node("PROXY", None, true, None),
        node("JP 01", Some("PROXY"), false, Some(20)),
        node("US 01", Some("PROXY"), false, Some(300)),
    ])));
    let _ = a.on_event(Event::Resize(80, 10));
    assert_eq!(a.nodes.selected_item().unwrap().name, "PROXY");
    let _ = press(&mut a, KeyCode::Char('k'));
    assert_eq!(a.nodes.selected_item().unwrap().name, "PROXY");
    let _ = press(&mut a, KeyCode::Char('G'));
    assert_eq!(a.nodes.selected_item().unwrap().name, "US 01");
    let _ = press(&mut a, KeyCode::Char('g'));
    assert_eq!(a.nodes.selected_item().unwrap().name, "PROXY");
    let _ = press(&mut a, KeyCode::Char('j'));
    let _ = press(&mut a, KeyCode::Char('j'));
    assert_eq!(a.nodes.selected_item().unwrap().name, "US 01");
}

#[test]
fn an_empty_list_absorbs_navigation_without_panicking() {
    let mut a = app();
    for screen in [
        Screen::Profiles,
        Screen::Proxies,
        Screen::Connections,
        Screen::Rules,
        Screen::Tests,
        Screen::Settings,
    ] {
        a.screen = screen;
        let _ = press(&mut a, KeyCode::Down);
        let _ = press(&mut a, KeyCode::Up);
        let _ = press(&mut a, KeyCode::PageDown);
        let _ = press(&mut a, KeyCode::Char('G'));
        let _ = press(&mut a, KeyCode::Enter);
        let _ = press(&mut a, KeyCode::Char('x'));
    }
    // Nothing above should have produced an effect or a panic.
    assert!(!a.is_quit());
}

#[test]
fn every_action_is_answered_with_an_effect_or_a_reason() {
    // The dispatcher must not have a silent arm: each action either does
    // something, asks a question, or explains why it cannot.
    for action in all_actions() {
        // Pure navigation is answered by the cursor moving rather than by
        // a message, and is covered by the movement tests; the contract
        // here is that every *other* action says or does something.
        if matches!(
            action,
            Action::Up
                | Action::Down
                | Action::PageUp
                | Action::PageDown
                | Action::Top
                | Action::Bottom
                | Action::SearchNext
        ) {
            let mut a = loaded();
            a.screen = Screen::Proxies;
            assert!(
                a.dispatch(action.clone(), true).is_empty(),
                "{action:?} must move the cursor, not fire an effect"
            );
            continue;
        }
        if action == Action::Cancel {
            let mut a = loaded();
            a.set_status(StatusKind::Error, "dismiss me");
            assert!(a.dispatch(action, true).is_empty());
            assert!(a.current_status().is_none());
            continue;
        }
        let mut a = loaded();
        let before = a.screen;
        let expanded_before = a.is_expanded("PROXY");
        let effects = a.dispatch(action.clone(), true);
        let answered = !effects.is_empty()
            || a.overlay.is_some()
            || a.current_status().is_some()
            || a.is_quit()
            || a.screen != before
            // Expanding a group is its own answer: it changes what the
            // proxies list shows without firing an effect.
            || a.is_expanded("PROXY") != expanded_before;
        assert!(answered, "{action:?} was answered with silence");
    }
}

/// Every action, so the check above cannot silently skip one.
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
        Action::CycleNodeSort,
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
        Action::CycleCoreMode,
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

/// The action keys documented in the footer must be the ones that fire.
#[test]
fn a_representative_key_on_every_screen_produces_exactly_one_effect() {
    let cases: [(Screen, KeyCode, Action); 10] = [
        (Screen::Home, KeyCode::Char('s'), Action::StartCore),
        (Screen::Profiles, KeyCode::Char('e'), Action::EditProfile),
        (Screen::Profiles, KeyCode::Char('p'), Action::PreviewConfig),
        (Screen::Proxies, KeyCode::Char('T'), Action::TestGroup),
        (
            Screen::Connections,
            KeyCode::Char('d'),
            Action::CloseConnection,
        ),
        (Screen::Logs, KeyCode::Char('l'), Action::CycleLogLevel),
        (Screen::Rules, KeyCode::Enter, Action::ToggleRule),
        (Screen::Tests, KeyCode::Enter, Action::RunTests),
        (Screen::Settings, KeyCode::Char('s'), Action::SaveSettings),
        (Screen::Home, KeyCode::Char('r'), Action::Refresh),
    ];
    for (screen, code, action) in cases {
        let mut a = loaded();
        a.screen = screen;
        let direct = a.dispatch(action.clone(), true);
        let mut b = loaded();
        b.screen = screen;
        let by_key = b.on_event(Event::Key(key(code)));
        assert_eq!(
            direct, by_key,
            "{code:?} on {screen} should mean {action:?}"
        );
        assert!(!direct.is_empty());
    }
}

#[test]
fn stopping_the_log_follow_freezes_the_window_without_losing_lines() {
    let mut a = loaded();
    goto(&mut a, Screen::Logs);
    for i in 0..5 {
        let _ = a.on_event(Event::Data(Data::Log(LogRow::new(
            "info",
            format!("line {i}"),
        ))));
    }
    let (window, hidden) = a.log_window(2);
    assert_eq!(hidden, 0);
    assert_eq!(
        window
            .iter()
            .map(|l| l.message.as_str())
            .collect::<Vec<_>>(),
        vec!["line 3", "line 4"],
        "the window shows the newest lines while following"
    );

    let _ = press(&mut a, KeyCode::Char('f'));
    for i in 5..9 {
        let _ = a.on_event(Event::Data(Data::Log(LogRow::new(
            "info",
            format!("line {i}"),
        ))));
    }
    let (frozen, hidden) = a.log_window(2);
    assert_eq!(
        frozen
            .iter()
            .map(|l| l.message.as_str())
            .collect::<Vec<_>>(),
        vec!["line 3", "line 4"],
        "a frozen view must not jump to the newest line"
    );
    assert_eq!(hidden, 4, "the hidden count is what the title reports");
    assert_eq!(a.logs.len(), 9, "nothing was discarded");

    let _ = press(&mut a, KeyCode::Char('f'));
    let (again, hidden) = a.log_window(1);
    assert_eq!(hidden, 0);
    assert_eq!(again[0].message, "line 8", "following resumes at the end");
}

#[test]
fn clearing_the_logs_also_clears_the_frozen_window() {
    let mut a = loaded();
    goto(&mut a, Screen::Logs);
    let _ = a.on_event(Event::Data(Data::Log(LogRow::new("info", "one"))));
    let _ = press(&mut a, KeyCode::Char('f'));
    let _ = press(&mut a, KeyCode::Char('c'));
    assert!(a.logs.is_empty());
    assert_eq!(a.log_window(10).1, 0);
}

#[test]
fn the_rule_count_reports_what_the_list_is_hiding() {
    let mut a = loaded();
    goto(&mut a, Screen::Rules);
    assert_eq!(a.rules.len(), 2);
    assert_eq!(a.hidden_rules(), 0);
    let _ = press(&mut a, KeyCode::Char('h'));
    assert_eq!(a.hidden_rules(), 1);
}

#[test]
fn the_active_filter_is_readable_from_outside_the_dispatcher() {
    let mut a = loaded();
    goto(&mut a, Screen::Profiles);
    assert!(a.active_filter().is_empty());
    press(&mut a, KeyCode::Char('/'));
    typed(&mut a, "tok");
    assert_eq!(a.active_filter(), "tok");
    // The filter belongs to the screen it was typed on.
    a.screen = Screen::Rules;
    assert!(a.active_filter().is_empty());
}

#[test]
fn a_settings_reload_replaces_the_theme_and_the_level() {
    let mut a = app();
    assert!(a.theme.color, "the constructed palette is used until then");
    let mut settings = Settings::default();
    settings.ui.color = false;
    settings.ui.log_level = LogLevel::Debug;
    let _ = a.on_event(Event::Data(Data::Settings(Box::new(settings))));
    assert!(!a.theme.color, "the colour setting rebuilds the palette");
    assert_eq!(a.log_level, LogLevel::Debug);
    assert!(!a.settings_dirty, "data from disk is not an unsaved edit");
}

#[test]
fn a_note_reaches_the_status_line_without_an_overlay() {
    let mut a = loaded();
    let _ = a.on_event(Event::Data(Data::Notice("3 warnings".to_owned())));
    assert!(a.overlay.is_none());
    assert_eq!(a.current_status().unwrap().text, "3 warnings");
}

#[test]
fn a_lost_core_stops_every_queued_test() {
    let mut a = loaded();
    goto(&mut a, Screen::Tests);
    let _ = press(&mut a, KeyCode::Enter);
    assert_eq!(a.queued_tests(), 1);
    let _ = a.on_event(Event::Data(Data::Core(CoreStatus::Stopped)));
    assert_eq!(a.queued_tests(), 0);
    assert!(
        a.tests
            .items()
            .iter()
            .all(|row| !matches!(row.result, TestResult::Running)),
        "a test cannot still be running once the core is gone"
    );
}

#[test]
fn unlock_checks_keep_the_cursor_when_proxy_data_refreshes() {
    let mut a = loaded();
    goto(&mut a, Screen::Tests);
    assert_eq!(a.tests.items()[0].target, "current route");
    let _ = press(&mut a, KeyCode::Down);
    let _ = press(&mut a, KeyCode::Down);
    assert_eq!(
        a.tests.selected_item().unwrap().kind,
        TestKind::Unlock("ChatGPT Web")
    );
    let _ = a.on_event(Event::Data(Data::Nodes(vec![node(
        "OFFICE", None, true, None,
    )])));
    assert_eq!(a.tests.items()[0].target, "current route");
    assert_eq!(
        a.tests.selected_item().unwrap().kind,
        TestKind::Unlock("ChatGPT Web")
    );
}

#[test]
fn the_editor_opens_the_runtime_configuration_the_core_was_launched_with() {
    let mut a = loaded();
    a.home = PathBuf::from("/srv/cvt");
    goto(&mut a, Screen::Home);
    assert_eq!(
        press(&mut a, KeyCode::Char('e')),
        vec![Effect::OpenEditor {
            path: PathBuf::from("/srv/cvt/runtime/config.yaml")
        }]
    );
}
