use super::*;

// -- tests screen -------------------------------------------------------

#[test]
fn the_test_queue_runs_one_at_a_time_and_cancels_as_a_batch() {
    let mut a = loaded();
    goto(&mut a, Screen::Tests);
    assert_eq!(a.tests.len(), 12, "one row per unlock service");

    let first = press(&mut a, KeyCode::Enter);
    assert_eq!(a.queued_tests(), 1);
    assert!(matches!(a.tests.items()[0].result, TestResult::Running));
    let target = a.tests.items()[0].target.clone();
    assert_eq!(
        first,
        vec![Effect::RunTest {
            kind: TestKind::Unlock("哔哩哔哩大陆"),
            target,
            mode: ProbeMode::Connect,
        }]
    );

    // Queueing a second test does not start it early.
    let _ = press(&mut a, KeyCode::Down);
    assert_eq!(press(&mut a, KeyCode::Enter), Vec::new());
    assert_eq!(a.queued_tests(), 2);
    assert!(a.current_status().unwrap().text.contains("queued"));

    // The answer to the first starts the second.
    let effects = a.on_event(Event::Data(Data::TestResult {
        mode: ProbeMode::Connect,
        kind: TestKind::Unlock("哔哩哔哩大陆"),
        target: a.tests.items()[0].target.clone(),
        result: TestResult::Passed("42 ms".to_owned()),
    }));
    assert_eq!(a.queued_tests(), 1);
    assert!(matches!(effects.as_slice(), [Effect::RunTest { .. }]));

    assert_eq!(press(&mut a, KeyCode::Char('s')), vec![Effect::CancelTests]);
    assert_eq!(a.queued_tests(), 0);
    assert_eq!(a.current_status().unwrap().kind, StatusKind::Warning);
}

#[test]
fn run_all_waits_for_completion_and_clear_resets_every_row() {
    let mut app = loaded();
    goto(&mut app, Screen::Tests);
    assert!(matches!(
        press(&mut app, KeyCode::Char('a')).as_slice(),
        [Effect::CancelTests, Effect::RunTest { .. }]
    ));
    assert_eq!(app.queued_tests(), 12);
    let first = app.tests.items()[0].clone();
    assert!(
        app.on_event(Event::Data(Data::TestResult {
            mode: ProbeMode::Connect,
            kind: first.kind,
            target: first.target.clone(),
            result: TestResult::Running
        }))
        .is_empty()
    );
    assert_eq!(app.queued_tests(), 12);
    assert!(matches!(
        app.on_event(Event::Data(Data::TestResult {
            mode: ProbeMode::Connect,
            kind: first.kind,
            target: first.target,
            result: TestResult::Passed("ok".to_owned())
        }))
        .as_slice(),
        [Effect::RunTest { .. }]
    ));
    assert_eq!(app.queued_tests(), 11);
    assert_eq!(
        press(&mut app, KeyCode::Char('c')),
        vec![Effect::ClearTestResults]
    );
    assert!(
        app.tests
            .items()
            .iter()
            .all(|row| row.result == TestResult::Pending)
    );
}

#[test]
fn a_test_that_has_run_is_not_queued_twice() {
    let mut a = loaded();
    goto(&mut a, Screen::Tests);
    let _ = press(&mut a, KeyCode::Enter);
    let _ = a.on_event(Event::Data(Data::TestResult {
        mode: ProbeMode::Connect,
        kind: TestKind::Unlock("哔哩哔哩大陆"),
        target: a.tests.items()[0].target.clone(),
        result: TestResult::Passed("12 ms".to_owned()),
    }));
    assert_eq!(press(&mut a, KeyCode::Enter), Vec::new());
    assert!(a.current_status().unwrap().text.contains("already run"));
    assert_eq!(
        press(&mut a, KeyCode::Char('c')),
        vec![Effect::ClearTestResults],
        "clearing puts it back to pending"
    );
    assert!(
        a.tests
            .items()
            .iter()
            .all(|r| r.result == TestResult::Pending)
    );
}

#[test]
fn a_finished_batch_preserves_source_order_until_the_user_changes_it() {
    let mut a = loaded();
    goto(&mut a, Screen::Proxies);
    press(&mut a, KeyCode::Enter);
    let _ = a.on_event(Event::Done(Done::NodeTestsFinished { tested: 2 }));
    assert_eq!(a.node_sort, SortOrder::Natural);
    assert_eq!(press(&mut a, KeyCode::Char('s')), Vec::new());
    assert_eq!(a.node_sort, SortOrder::LatencyAscending);
    let names: Vec<&str> = a.nodes.items().iter().map(|r| r.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["PROXY", "JP 01", "US 01"],
        "the group stays on top and its members are ordered fastest first"
    );
}

#[test]
fn live_probe_updates_only_members_and_survives_inventory_refresh() {
    let mut a = loaded();
    goto(&mut a, Screen::Proxies);
    let inventory = vec![
        node("A", None, true, None),
        node("a-slow", Some("A"), false, Some(90)),
        node("a-fast", Some("A"), false, Some(20)),
        node("B", None, true, None),
        node("b-fast", Some("B"), false, Some(5)),
        node("b-slow", Some("B"), false, Some(70)),
    ];
    let _ = a.on_event(Event::Data(Data::Nodes(inventory.clone())));
    a.expanded = vec!["A".to_owned(), "B".to_owned()];
    a.rebuild_nodes();
    let _ = a.on_event(Event::Done(Done::NodeTestsFinished { tested: 4 }));
    press(&mut a, KeyCode::Char('s'));
    let names = |app: &App| {
        app.nodes
            .items()
            .iter()
            .map(|row| row.name.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        names(&a),
        ["A", "a-fast", "a-slow", "B", "b-fast", "b-slow"]
    );
    let _ = a.on_event(Event::Data(Data::NodeDelay {
        mode: ProbeMode::Connect,
        name: "a-slow".to_owned(),
        delay: Some(1),
    }));
    assert_eq!(
        names(&a),
        ["A", "a-slow", "a-fast", "B", "b-fast", "b-slow"]
    );
    let _ = a.on_event(Event::Data(Data::Nodes(inventory)));
    assert_eq!(
        names(&a),
        ["A", "a-slow", "a-fast", "B", "b-fast", "b-slow"]
    );
    assert_eq!(a.nodes.items()[1].delay, Some(1));
    assert_eq!(
        press(&mut a, KeyCode::Char('v')),
        vec![Effect::Refresh(Screen::Proxies)]
    );
    assert_eq!(a.probe_mode, ProbeMode::Tcp);
    assert_eq!(a.nodes.items()[1].delay, None);
}

#[test]
fn unlock_targets_remain_the_current_route_when_proxy_groups_change() {
    let mut a = loaded();
    goto(&mut a, Screen::Tests);
    let _ = a.on_event(Event::Data(Data::Nodes(vec![node("A", None, true, None)])));
    assert!(
        a.tests
            .items()
            .iter()
            .all(|row| row.target == "current route")
    );
    assert_eq!(a.tests.items().len(), 12);
}
