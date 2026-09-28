use super::*;

// -- core ---------------------------------------------------------------

#[test]
fn the_home_screen_carries_the_core_actions() {
    let mut a = loaded();
    goto(&mut a, Screen::Home);
    assert_eq!(press(&mut a, KeyCode::Char('R')), vec![Effect::RestartCore]);
    assert_eq!(press(&mut a, KeyCode::Char('g')), vec![Effect::UpdateGeo]);
    assert_eq!(press(&mut a, KeyCode::Char('F')), vec![Effect::FlushCaches]);
    assert_eq!(
        press(&mut a, KeyCode::Char('e')),
        vec![Effect::OpenEditor {
            path: a.home.join("runtime").join("config.yaml")
        }]
    );
    assert_eq!(press(&mut a, KeyCode::Char('S')), Vec::new());
    assert!(matches!(a.overlay, Some(Overlay::Confirm { .. })));
    assert_eq!(typed(&mut a, "y"), vec![Effect::StopCore]);
}

#[test]
fn starting_the_core_does_not_ask_because_it_destroys_nothing() {
    let mut a = loaded();
    goto(&mut a, Screen::Home);
    assert_eq!(press(&mut a, KeyCode::Char('s')), vec![Effect::StartCore]);
    assert!(a.overlay.is_none());
}
