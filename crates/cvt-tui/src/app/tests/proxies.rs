use super::*;

// -- proxies ------------------------------------------------------------

#[test]
fn switching_a_profile_replaces_stale_proxy_data_and_prepares_or_applies() {
    let mut stopped = loaded();
    stopped.core = CoreStatus::Stopped;
    let effects = stopped.on_event(Event::Done(Done::ProfileSwitched {
        name: "new".to_owned(),
    }));
    assert!(stopped.nodes.is_empty());
    assert!(effects.contains(&Effect::PrepareConfig));
    assert!(effects.contains(&Effect::LoadProfiles));

    let mut running = loaded();
    let effects = running.on_event(Event::Done(Done::ProfileSwitched {
        name: "new".to_owned(),
    }));
    assert!(running.nodes.is_empty());
    assert!(effects.contains(&Effect::ApplyConfig {
        mode: ReloadMode::Auto,
    }));
}

#[test]
fn deleting_the_current_profile_clears_proxies_and_stops_its_core() {
    let mut a = loaded();
    let effects = a.on_event(Event::Done(Done::ProfileDeleted {
        name: "base".to_owned(),
        was_current: true,
    }));
    assert!(a.nodes.is_empty());
    assert!(effects.contains(&Effect::StopCore));
}

#[test]
fn routing_mode_cycles_through_all_three_core_modes() {
    let mut a = loaded();
    goto(&mut a, Screen::Home);
    a.core_mode = Some("rule".to_owned());
    assert_eq!(
        press(&mut a, KeyCode::Char('M')),
        vec![Effect::SetCoreMode {
            mode: "global".to_owned(),
        }]
    );
    let _ = a.on_event(Event::Done(Done::CoreModeChanged {
        mode: "global".to_owned(),
    }));
    assert_eq!(
        press(&mut a, KeyCode::Char('M')),
        vec![Effect::SetCoreMode {
            mode: "direct".to_owned(),
        }]
    );
    let _ = a.on_event(Event::Done(Done::CoreModeChanged {
        mode: "direct".to_owned(),
    }));
    assert_eq!(
        press(&mut a, KeyCode::Char('M')),
        vec![Effect::SetCoreMode {
            mode: "rule".to_owned(),
        }]
    );
}

#[test]
fn a_group_row_expands_and_collapses_its_members() {
    let mut a = loaded();
    goto(&mut a, Screen::Proxies);
    assert_eq!(a.nodes.len(), 1, "groups start collapsed");
    assert_eq!(press(&mut a, KeyCode::Enter), Vec::new());
    assert_eq!(a.nodes.len(), 3);
    assert!(a.is_expanded("PROXY"));
    assert_eq!(press(&mut a, KeyCode::Enter), Vec::new());
    assert_eq!(a.nodes.len(), 1, "pressing again collapses it");
    assert!(!a.is_expanded("PROXY"));
}

#[test]
fn a_member_row_is_pinned_in_its_group() {
    let mut a = loaded();
    goto(&mut a, Screen::Proxies);
    press(&mut a, KeyCode::Enter); // expand
    let _ = press(&mut a, KeyCode::Down); // onto JP 01
    assert_eq!(
        a.nodes.selected_item().unwrap().name,
        "JP 01",
        "the member is highlighted"
    );
    assert_eq!(
        press(&mut a, KeyCode::Enter),
        vec![Effect::SelectNode {
            group: "PROXY".to_owned(),
            member: "JP 01".to_owned()
        }]
    );
}

#[test]
fn successful_node_selection_updates_the_visible_row_and_refreshes() {
    let mut a = loaded();
    goto(&mut a, Screen::Proxies);
    press(&mut a, KeyCode::Enter);
    let effects = a.on_event(Event::Done(Done::NodeSelected {
        group: "PROXY".to_owned(),
        member: "US 01".to_owned(),
    }));
    assert_eq!(
        effects,
        vec![Effect::Refresh(Screen::Proxies), Effect::RefreshIp]
    );
    assert!(
        a.nodes
            .items()
            .iter()
            .any(|row| row.name == "US 01" && row.active)
    );
    assert!(
        a.nodes
            .items()
            .iter()
            .any(|row| row.name == "JP 01" && !row.active)
    );
}

#[test]
fn testing_reads_the_highlighted_row_the_way_the_proxies_screen_shows_it() {
    let mut a = loaded();
    goto(&mut a, Screen::Proxies);
    // A group row: `t` tests the group, `T` tests the group it belongs to.
    assert_eq!(
        press(&mut a, KeyCode::Char('t')),
        vec![Effect::TestGroup {
            group: "PROXY".to_owned(),
            mode: ProbeMode::Connect,
        }]
    );
    press(&mut a, KeyCode::Enter);
    let _ = press(&mut a, KeyCode::Down);
    assert_eq!(
        press(&mut a, KeyCode::Char('t')),
        vec![Effect::TestNode {
            name: "JP 01".to_owned(),
            mode: ProbeMode::Connect,
        }]
    );
    assert_eq!(
        press(&mut a, KeyCode::Char('T')),
        vec![Effect::TestGroup {
            group: "PROXY".to_owned(),
            mode: ProbeMode::Connect,
        }]
    );
    assert_eq!(
        press(&mut a, KeyCode::Char('a')),
        vec![Effect::TestAllNodes {
            mode: ProbeMode::Connect
        }]
    );
}

#[test]
fn clearing_a_pin_names_the_group_that_owns_the_row() {
    let mut a = loaded();
    goto(&mut a, Screen::Proxies);
    assert_eq!(
        press(&mut a, KeyCode::Char('x')),
        vec![Effect::ClearNodePin {
            group: "PROXY".to_owned()
        }]
    );
    press(&mut a, KeyCode::Enter);
    let _ = press(&mut a, KeyCode::Down);
    assert_eq!(
        press(&mut a, KeyCode::Char('x')),
        vec![Effect::ClearNodePin {
            group: "PROXY".to_owned()
        }]
    );
}

#[test]
fn a_core_action_needs_a_running_core_and_says_so() {
    let mut a = app();
    let _ = a.on_event(Event::Data(Data::Nodes(vec![
        node("PROXY", None, true, None),
        node("JP 01", Some("PROXY"), false, Some(20)),
    ])));
    goto(&mut a, Screen::Proxies);
    press(&mut a, KeyCode::Enter);
    let _ = press(&mut a, KeyCode::Down);
    assert_eq!(press(&mut a, KeyCode::Enter), Vec::new());
    let warning = a.current_status().unwrap();
    assert_eq!(warning.kind, StatusKind::Warning);
    assert!(warning.text.contains("running core"), "{}", warning.text);
}
