use super::*;

// -- logs ---------------------------------------------------------------

#[test]
fn the_log_screen_controls_the_level_follow_and_export() {
    let mut a = loaded();
    goto(&mut a, Screen::Logs);
    assert_eq!(
        press(&mut a, KeyCode::Char('l')),
        vec![Effect::SetCoreLogLevel(LogLevel::Debug)],
        "the levels cycle quietest last: Info is followed by Debug"
    );
    assert_eq!(a.log_level, LogLevel::Debug);
    assert!(a.settings_dirty, "the level is part of the settings");

    let _ = press(&mut a, KeyCode::Char('f'));
    assert!(!a.logs.follow);
    let _ = press(&mut a, KeyCode::Char('f'));
    assert!(a.logs.follow);

    assert_eq!(press(&mut a, KeyCode::Char('x')), Vec::new());
    assert!(a.current_status().unwrap().text.contains("empty"));

    for i in 0..3 {
        let _ = a.on_event(Event::Data(Data::Log(LogRow::new(
            "info",
            format!("line {i}"),
        ))));
    }
    let effects = press(&mut a, KeyCode::Char('x'));
    match effects.as_slice() {
        [Effect::ExportLogs { path, contents }] => {
            assert!(path.starts_with(&a.home));
            assert!(path.to_string_lossy().contains("logs"));
            assert_eq!(contents.lines().count(), 3);
        }
        other => panic!("expected an export, got {other:?}"),
    }
    let _ = press(&mut a, KeyCode::Char('c'));
    assert!(a.logs.is_empty());
}

#[test]
fn the_log_level_is_not_sent_to_a_core_that_is_not_running() {
    let mut a = app();
    goto(&mut a, Screen::Logs);
    assert_eq!(press(&mut a, KeyCode::Char('l')), Vec::new());
    assert!(a.current_status().unwrap().text.contains("not running"));
}
