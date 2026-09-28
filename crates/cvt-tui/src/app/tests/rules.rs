use super::*;

// -- rules --------------------------------------------------------------

#[test]
fn a_rule_is_toggled_the_other_way_and_disabled_rules_remain_reachable() {
    let mut a = loaded();
    goto(&mut a, Screen::Rules);
    assert_eq!(a.rules.len(), 2, "disabled rules are visible by default");
    assert_eq!(
        press(&mut a, KeyCode::Enter),
        vec![Effect::ToggleRule {
            index: 0,
            disabled: true
        }]
    );
    assert_eq!(press(&mut a, KeyCode::Char('h')), Vec::new());
    assert!(!a.show_disabled_rules);
    assert_eq!(a.rules.len(), 1);
    assert_eq!(
        press(&mut a, KeyCode::Char(' ')),
        vec![Effect::ToggleRule {
            index: 0,
            disabled: true
        }]
    );
}

#[test]
fn successful_rule_toggle_updates_the_visible_row_before_refresh() {
    let mut a = loaded();
    goto(&mut a, Screen::Rules);
    assert!(!a.rules.items()[0].disabled);
    assert_eq!(
        a.on_event(Event::Done(Done::RuleToggled {
            index: 0,
            disabled: true,
        })),
        vec![Effect::Refresh(Screen::Rules)]
    );
    assert!(a.rules.items()[0].disabled);
    assert_eq!(
        press(&mut a, KeyCode::Enter),
        vec![Effect::ToggleRule {
            index: 0,
            disabled: false,
        }]
    );
}

#[test]
fn successful_connection_close_updates_the_visible_list_before_refresh() {
    let mut a = loaded();
    goto(&mut a, Screen::Connections);
    assert_eq!(a.connections.len(), 2);
    assert_eq!(
        a.on_event(Event::Done(Done::ConnectionClosed {
            id: "c1".to_owned()
        })),
        vec![Effect::Refresh(Screen::Connections)]
    );
    assert_eq!(a.connections.len(), 1);
    assert_eq!(a.connections.items()[0].id, "c2");
    assert_eq!(
        a.on_event(Event::Done(Done::ConnectionsClosed { count: 1 })),
        vec![Effect::Refresh(Screen::Connections)]
    );
    assert!(a.connections.is_empty());
}

#[test]
fn updating_one_rule_set_falls_back_to_every_set_when_the_mapping_is_unknown() {
    let mut a = loaded();
    goto(&mut a, Screen::Rules);
    assert_eq!(
        press(&mut a, KeyCode::Char('u')),
        vec![Effect::UpdateRuleProviders { names: Vec::new() }],
        "a rule row carries no provider, so the request has to widen"
    );
    assert!(a.current_status().unwrap().text.contains("every set"));
    assert_eq!(
        press(&mut a, KeyCode::Char('U')),
        vec![Effect::UpdateRuleProviders { names: Vec::new() }]
    );

    // Once the core names its providers, a single one can be chosen.
    let _ = a.on_event(Event::Data(Data::RuleProviders(vec!["geosite".to_owned()])));
    assert_eq!(
        press(&mut a, KeyCode::Char('u')),
        vec![Effect::UpdateRuleProviders {
            names: vec!["geosite".to_owned()]
        }]
    );
}

#[test]
fn proxy_speed_key_reports_the_current_route_without_changing_selection() {
    let mut app = loaded();
    goto(&mut app, Screen::Proxies);
    let before = app.nodes.selected_item().map(|row| row.name.clone());
    assert!(press(&mut app, KeyCode::Char('b')).is_empty());
    assert!(matches!(app.overlay, Some(Overlay::Picker { .. })));
    assert_eq!(
        press(&mut app, KeyCode::Enter),
        vec![Effect::TestRouteSpeed {
            mode: SpeedMode::Sample4
        }]
    );
    let _ = app.on_event(Event::Data(Data::RouteSpeed(Ok(
        "42.0 Mbit/s (speedtest-go)".to_owned(),
    ))));
    assert_eq!(
        app.route_speed.as_deref(),
        Some("42.0 Mbit/s (speedtest-go)")
    );
    assert_eq!(
        app.nodes.selected_item().map(|row| row.name.clone()),
        before
    );
}

#[test]
fn bandwidth_picker_remembers_size_and_asks_before_installing_backend() {
    let mut app = loaded();
    goto(&mut app, Screen::Proxies);
    assert!(press(&mut app, KeyCode::Char('b')).is_empty());
    let _ = press(&mut app, KeyCode::Down);
    let _ = press(&mut app, KeyCode::Down);
    assert_eq!(
        press(&mut app, KeyCode::Enter),
        vec![Effect::TestRouteSpeed {
            mode: SpeedMode::Sample100
        }]
    );
    assert_eq!(app.speed_mode, SpeedMode::Sample100);
    let _ = press(&mut app, KeyCode::Char('b'));
    assert!(matches!(
        app.overlay,
        Some(Overlay::Picker { selected: 2, .. })
    ));
    let _ = press(&mut app, KeyCode::Down);
    assert_eq!(
        press(&mut app, KeyCode::Enter),
        vec![Effect::TestRouteSpeed {
            mode: SpeedMode::Speedtest
        }]
    );
    assert!(app.on_event(Event::Data(Data::SpeedtestMissing)).is_empty());
    assert!(matches!(
        app.overlay,
        Some(Overlay::Confirm {
            action: Action::InstallSpeedtestGo,
            ..
        })
    ));
    assert_eq!(press(&mut app, KeyCode::Char('n')), Vec::new());
    assert!(app.overlay.is_none());
}

#[test]
fn connect_result_changes_unavailable_node_health_until_it_expires() {
    let mut app = loaded();
    goto(&mut app, Screen::Proxies);
    let mut rows = app.all_nodes.clone();
    rows[1].alive = false;
    let _ = app.on_event(Event::Data(Data::Nodes(rows.clone())));
    app.expanded.push("PROXY".to_owned());
    app.rebuild_nodes();
    assert!(
        !app.nodes
            .items()
            .iter()
            .find(|row| row.name == "JP 01")
            .unwrap()
            .alive
    );
    let _ = app.on_event(Event::Data(Data::NodeDelay {
        mode: ProbeMode::Connect,
        name: "JP 01".to_owned(),
        delay: Some(42),
    }));
    assert!(
        app.nodes
            .items()
            .iter()
            .find(|row| row.name == "JP 01")
            .unwrap()
            .alive
    );
    let _ = app.on_event(Event::Data(Data::Nodes(rows)));
    assert!(
        app.nodes
            .items()
            .iter()
            .find(|row| row.name == "JP 01")
            .unwrap()
            .alive
    );
    app.node_health.insert(
        "JP 01".to_owned(),
        (true, Instant::now().checked_sub(NODE_HEALTH_TTL).unwrap()),
    );
    let _ = app.on_tick();
    assert!(
        !app.nodes
            .items()
            .iter()
            .find(|row| row.name == "JP 01")
            .unwrap()
            .alive
    );
}

#[test]
fn home_refresh_and_lookup_failure_have_visible_state() {
    let mut app = loaded();
    goto(&mut app, Screen::Home);
    assert_eq!(press(&mut app, KeyCode::Char('r')), vec![Effect::RefreshIp]);
    let _ = app.on_event(Event::Data(Data::IpLookupStarted));
    assert!(app.ip_refreshing);
    let _ = app.on_event(Event::Data(Data::IpLookupFailed(
        "service unavailable".to_owned(),
    )));
    assert!(!app.ip_refreshing);
    assert_eq!(app.ip_error.as_deref(), Some("service unavailable"));
    assert!(
        app.current_status()
            .unwrap()
            .text
            .contains("service unavailable")
    );
}
