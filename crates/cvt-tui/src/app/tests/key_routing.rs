use super::*;

// -- key routing --------------------------------------------------------

#[test]
fn a_quit_key_stops_the_application() {
    let mut a = app();
    assert!(!a.is_quit());
    assert_eq!(press(&mut a, KeyCode::Char('q')), vec![Effect::Quit]);
    assert!(a.is_quit());
}

#[test]
fn ctrl_c_quits_from_every_screen() {
    for screen in Screen::all() {
        let mut a = app();
        goto(&mut a, Screen::Home);
        a.screen = screen;
        let effects = a.on_event(Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        )));
        assert_eq!(effects, vec![Effect::Quit], "{screen}");
    }
}

#[test]
fn an_overlay_takes_the_key_before_the_key_map_does() {
    let mut a = loaded();
    goto(&mut a, Screen::Profiles);
    assert_eq!(press(&mut a, KeyCode::Char('/')), Vec::new());
    // `q` types a `q` into the prompt instead of quitting.
    assert_eq!(typed(&mut a, "q"), Vec::new());
    assert!(!a.is_quit(), "a prompt must absorb `q`");
    assert_eq!(
        a.overlay,
        Some(Overlay::Prompt {
            label: "filter".to_owned(),
            kind: PromptKind::Search,
            value: "q".to_owned(),
            cursor: 1,
        })
    );
    // `Esc` closes it, and the key map is live again.
    assert_eq!(press(&mut a, KeyCode::Esc), Vec::new());
    assert!(a.overlay.is_none());
    assert_eq!(press(&mut a, KeyCode::Char('q')), vec![Effect::Quit]);
}

#[test]
fn a_confirmation_swallows_every_other_key() {
    let mut a = loaded();
    goto(&mut a, Screen::Profiles);
    press(&mut a, KeyCode::Char('d'));
    assert!(matches!(a.overlay, Some(Overlay::Confirm { .. })));
    // Neither `q` nor an unbound key may escape the question.
    assert_eq!(typed(&mut a, "qz"), Vec::new());
    assert!(!a.is_quit());
    assert!(matches!(a.overlay, Some(Overlay::Confirm { .. })));
}

#[test]
fn a_prompt_edits_at_the_caret_and_handles_multibyte_text() {
    let mut a = loaded();
    goto(&mut a, Screen::Profiles);
    press(&mut a, KeyCode::Char('/'));
    typed(&mut a, "東京");
    typed(&mut a, "x");
    assert_eq!(press(&mut a, KeyCode::Backspace), Vec::new());
    assert_eq!(press(&mut a, KeyCode::Left), Vec::new());
    typed(&mut a, "y");
    let Some(Overlay::Prompt { value, cursor, .. }) = &a.overlay else {
        panic!("the prompt should still be open");
    };
    assert_eq!(value, "東y京");
    assert_eq!(*cursor, 2, "the caret is counted in characters");
}

#[test]
fn a_prompt_can_be_cleared_and_home_and_end_move_the_caret() {
    let mut a = loaded();
    goto(&mut a, Screen::Profiles);
    press(&mut a, KeyCode::Char('/'));
    typed(&mut a, "abc");
    assert_eq!(press(&mut a, KeyCode::Home), Vec::new());
    let Some(Overlay::Prompt { cursor, .. }) = &a.overlay else {
        panic!("prompt");
    };
    assert_eq!(*cursor, 0);
    let _ = a.on_event(Event::Key(KeyEvent::new(
        KeyCode::Char('u'),
        KeyModifiers::CONTROL,
    )));
    let Some(Overlay::Prompt { value, cursor, .. }) = &a.overlay else {
        panic!("prompt");
    };
    assert!(value.is_empty());
    assert_eq!(*cursor, 0);
}
