use super::*;

// -- settings -----------------------------------------------------------

/// A typed-in number is held to the same ceiling a save is.
///
/// The class the ninth review found for the command line, one crate over:
/// `--timeout 32768` was refused by the settings and accepted by the flag.
/// Here the flag's equivalent is the prompt, and it accepted any parseable
/// number — which then sat in memory until a save failed.
#[test]
fn a_typed_in_number_the_settings_refuse_is_refused_at_the_prompt() {
    let mut settings = Settings::default();
    let before = settings.test.timeout_ms;

    let refused = set_setting_text(&mut settings, "test.timeout_ms", "99999");
    assert!(refused.is_err(), "the core parses this as an int16");
    assert_eq!(
        settings.test.timeout_ms, before,
        "and the value is rolled back rather than kept"
    );

    // The ceiling itself is accepted, so the check is the settings' and not
    // a second, stricter one written here.
    assert!(
        set_setting_text(
            &mut settings,
            "test.timeout_ms",
            &cvt_core::settings::MAX_TEST_TIMEOUT_MS.to_string()
        )
        .is_ok()
    );

    // And the same for the other ceiling, and for a value that is not a
    // number at all.
    assert!(set_setting_text(&mut settings, "test.concurrency", "0").is_err());
    assert!(set_setting_text(&mut settings, "logs.keep", "65").is_err());
    assert!(set_setting_text(&mut settings, "test.timeout_ms", "soon").is_err());
}

#[test]
fn setting_switches_toggle_and_choices_open_a_picker() {
    let mut a = loaded();
    goto(&mut a, Screen::Settings);
    assert_eq!(a.settings_rows.len(), 27);
    a.settings_rows
        .select_by_key("ui.color".to_owned(), |row| row.key.to_owned());
    assert_eq!(press(&mut a, KeyCode::Enter), Vec::new());
    assert!(!a.settings.ui.color);
    assert!(a.settings_dirty);
    assert!(!a.theme.color, "the theme follows the setting");
    assert!(
        a.current_status().unwrap().text.contains("colour"),
        "the status names the row that changed"
    );

    a.settings_rows
        .select_by_key("core.use_managed".to_owned(), |row| row.key.to_owned());
    let _ = press(&mut a, KeyCode::Enter);
    assert!(!a.settings.core.use_managed);

    a.settings_rows
        .select_by_key("test.concurrency".to_owned(), |row| row.key.to_owned());
    let _ = press(&mut a, KeyCode::Enter);
    assert!(matches!(a.overlay, Some(Overlay::Picker { .. })));
    assert_eq!(a.settings.test.concurrency, 16);
    let _ = press(&mut a, KeyCode::Down);
    let _ = press(&mut a, KeyCode::Enter);
    assert_eq!(a.settings.test.concurrency, 32);
    let _ = press(&mut a, KeyCode::Char(' '));
    let _ = press(&mut a, KeyCode::End);
    let _ = press(&mut a, KeyCode::Enter);
    assert_eq!(a.settings.test.concurrency, 64);
}

#[test]
fn changing_language_rebuilds_settings_rows_immediately() {
    let mut app = loaded();
    goto(&mut app, Screen::Settings);
    app.settings_rows
        .select_by_key("ui.language".to_owned(), |row| row.key.to_owned());
    let _ = press(&mut app, KeyCode::Enter);
    let _ = press(&mut app, KeyCode::Down);
    let _ = press(&mut app, KeyCode::Enter);
    assert_eq!(app.settings.ui.language, Language::Chinese);
    assert!(app.settings_dirty);
    let row = app.settings_rows.selected_item().expect("language row");
    assert_eq!(row.key, "ui.language");
    assert_eq!(row.label, "界面语言");
    assert_eq!(row.value, "简体中文");
    let _ = press(&mut app, KeyCode::Enter);
    let _ = press(&mut app, KeyCode::Up);
    let _ = press(&mut app, KeyCode::Enter);
    assert_eq!(app.settings.ui.language, Language::English);
    assert_eq!(app.settings_rows.selected_item().unwrap().label, "language");
}

#[test]
fn a_text_setting_is_edited_in_a_prompt_and_validated() {
    let mut a = loaded();
    goto(&mut a, Screen::Settings);
    a.settings_rows
        .select_by_key("test.url".to_owned(), |row| row.key.to_owned());
    let _ = press(&mut a, KeyCode::Enter);
    let Some(Overlay::Prompt { label, value, .. }) = &a.overlay else {
        panic!("a prompt for the URL");
    };
    assert_eq!(label, "latency test URL");
    assert!(
        !value.is_empty(),
        "the prompt starts from the current value"
    );

    let _ = a.on_event(Event::Key(KeyEvent::new(
        KeyCode::Char('u'),
        KeyModifiers::CONTROL,
    )));
    typed(&mut a, "nonsense");
    assert_eq!(press(&mut a, KeyCode::Enter), Vec::new());
    assert!(
        a.overlay.is_some(),
        "an invalid value keeps the prompt open"
    );
    assert!(a.current_status().unwrap().text.contains("http"));

    let _ = a.on_event(Event::Key(KeyEvent::new(
        KeyCode::Char('u'),
        KeyModifiers::CONTROL,
    )));
    typed(&mut a, "https://example.com/generate_204");
    assert_eq!(press(&mut a, KeyCode::Enter), Vec::new());
    assert!(a.overlay.is_none());
    assert_eq!(a.settings.test.url, "https://example.com/generate_204");
    assert!(a.settings_dirty);
}

#[test]
fn saving_the_settings_sends_them_and_clears_the_dirty_flag() {
    let mut a = loaded();
    goto(&mut a, Screen::Settings);
    a.settings_rows
        .select_by_key("ui.show_footer".to_owned(), |row| row.key.to_owned());
    let _ = press(&mut a, KeyCode::Enter);
    assert!(a.settings_dirty);
    let effects = press(&mut a, KeyCode::Char('s'));
    match effects.as_slice() {
        [Effect::SaveSettings { settings }] => assert!(!settings.ui.show_footer),
        other => panic!("expected a save, got {other:?}"),
    }
    let _ = a.on_event(Event::Done(Done::SettingsSaved));
    assert!(!a.settings_dirty);
}

#[test]
fn the_settings_screen_keeps_its_place_when_a_value_changes() {
    let mut a = loaded();
    goto(&mut a, Screen::Settings);
    a.settings_rows
        .select_by_key("stream.logs".to_owned(), |row| row.key.to_owned());
    let _ = press(&mut a, KeyCode::Enter);
    assert_eq!(
        a.settings_rows.selected_item().unwrap().key,
        "stream.logs",
        "rebuilding the rows must not move the cursor"
    );
}
