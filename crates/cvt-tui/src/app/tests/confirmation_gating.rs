use super::*;

// -- confirmation gating ------------------------------------------------

#[test]
fn every_destructive_action_asks_first_and_only_runs_on_a_yes() {
    for action in [
        Action::DeleteProfile,
        Action::CloseAllConnections,
        Action::StopCore,
        Action::RollbackConfig,
        Action::UpgradeCore,
    ] {
        let mut a = loaded();
        goto(&mut a, Screen::Profiles);
        assert_eq!(a.dispatch(action.clone(), false), Vec::new());
        match &a.overlay {
            Some(Overlay::Confirm {
                question,
                action: held,
            }) => {
                assert!(!question.is_empty());
                assert_eq!(held, &action);
            }
            other => panic!("{action:?} should have asked, got {other:?}"),
        }
        // "no" leaves nothing behind.
        let _ = typed(&mut a, "n");
        assert!(a.overlay.is_none());
        // "yes" dispatches exactly once.
        let _ = a.dispatch(action.clone(), false);
        let effects = typed(&mut a, "y");
        assert_eq!(effects.len(), 1, "{action:?} should run once");
        assert!(a.overlay.is_none());
    }
}

#[test]
fn a_destructive_action_that_cannot_do_anything_says_so_instead_of_asking() {
    let mut a = app();
    // Nothing is running, so there is nothing to stop.
    assert!(a.dispatch(Action::StopCore, false).is_empty());
    assert!(a.overlay.is_none(), "no pointless confirmation");
    assert_eq!(a.current_status().unwrap().kind, StatusKind::Warning);
}

#[test]
fn delete_profile_really_deletes_the_highlighted_one() {
    let mut a = loaded();
    goto(&mut a, Screen::Profiles);
    a.profiles
        .select_by_key("patch".to_owned(), |r| r.uid.clone());
    let _ = press(&mut a, KeyCode::Char('d'));
    assert_eq!(
        typed(&mut a, "y"),
        vec![Effect::DeleteProfile {
            uid: "patch".to_owned()
        }]
    );
}
