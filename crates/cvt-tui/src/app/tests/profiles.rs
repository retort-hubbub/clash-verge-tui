use super::*;

// -- profiles -----------------------------------------------------------

#[test]
fn a_base_profile_cannot_be_chained_and_says_why() {
    let mut a = loaded();
    goto(&mut a, Screen::Profiles);
    a.profiles
        .select_by_key("base".to_owned(), |r| r.uid.clone());
    assert_eq!(press(&mut a, KeyCode::Char('c')), Vec::new());
    let warning = a.current_status().unwrap();
    assert_eq!(warning.kind, StatusKind::Warning);
    assert!(warning.text.contains("base document"), "{}", warning.text);
    assert!(a.chain().is_empty());
}

#[test]
fn chaining_a_patch_toggles_it_and_saves_the_chain() {
    let mut a = loaded();
    goto(&mut a, Screen::Profiles);
    a.profiles
        .select_by_key("patch".to_owned(), |r| r.uid.clone());
    assert_eq!(
        press(&mut a, KeyCode::Char('c')),
        vec![Effect::SetChain {
            uids: vec!["patch".to_owned()]
        }]
    );
    assert_eq!(a.chain(), ["patch"]);
    assert!(
        a.profiles
            .items()
            .iter()
            .find(|r| r.uid == "patch")
            .unwrap()
            .in_chain
    );
    // Pressing it again removes the patch from the chain.
    assert_eq!(
        press(&mut a, KeyCode::Char('c')),
        vec![Effect::SetChain { uids: Vec::new() }]
    );
    assert!(a.chain().is_empty());
}

#[test]
fn a_local_profile_has_nothing_to_download() {
    let mut a = loaded();
    goto(&mut a, Screen::Profiles);
    a.profiles
        .select_by_key("base".to_owned(), |r| r.uid.clone());
    assert_eq!(press(&mut a, KeyCode::Char('u')), Vec::new());
    assert!(
        a.current_status().unwrap().text.contains("local"),
        "the reason has to name the problem"
    );
}

#[test]
fn update_all_delegates_due_selection_to_the_executor() {
    let mut a = loaded();
    goto(&mut a, Screen::Profiles);
    let effects = press(&mut a, KeyCode::Char('U'));
    assert_eq!(
        effects,
        vec![Effect::UpdateProfiles { uids: Vec::new() }],
        "the executor checks the full index for due profiles"
    );

    let _ = a.on_event(Event::Data(Data::Profiles(vec![
        ProfileRow::from_item(&PrfItem::local("base", "base"), true, false),
        profile("r1", "Tokyo", Some("https://sub.example/tokyo")),
        profile("r2", "Berlin", Some("https://sub.example/berlin")),
    ])));
    assert_eq!(
        press(&mut a, KeyCode::Char('U')),
        vec![Effect::UpdateProfiles { uids: Vec::new() }]
    );
}

#[test]
fn the_new_profile_picker_leads_to_a_url_prompt() {
    let mut a = loaded();
    goto(&mut a, Screen::Profiles);
    assert_eq!(press(&mut a, KeyCode::Char('a')), Vec::new());
    assert!(matches!(a.overlay, Some(Overlay::Picker { .. })));
    // Enter picks "from a URL", which asks for the URL.
    assert_eq!(press(&mut a, KeyCode::Enter), Vec::new());
    assert!(matches!(
        a.overlay,
        Some(Overlay::Prompt {
            kind: PromptKind::Url,
            ..
        })
    ));
    typed(&mut a, "https://sub.example/tokyo");
    assert_eq!(
        press(&mut a, KeyCode::Enter),
        vec![Effect::NewProfile {
            name: "sub.example".to_owned(),
            url: Some("https://sub.example/tokyo".to_owned()),
        }]
    );
}

#[test]
fn a_blank_local_profile_asks_for_a_name() {
    let mut a = loaded();
    goto(&mut a, Screen::Profiles);
    press(&mut a, KeyCode::Char('a'));
    let _ = press(&mut a, KeyCode::Down);
    let _ = press(&mut a, KeyCode::Enter);
    typed(&mut a, "office");
    assert_eq!(
        press(&mut a, KeyCode::Enter),
        vec![Effect::NewProfile {
            name: "office".to_owned(),
            url: None
        }]
    );
}

#[test]
fn a_subscription_url_that_is_not_a_url_is_refused_and_the_prompt_stays() {
    let mut a = loaded();
    goto(&mut a, Screen::Profiles);
    press(&mut a, KeyCode::Char('a'));
    let _ = press(&mut a, KeyCode::Enter);
    typed(&mut a, "ftp://example.com/x");
    assert_eq!(press(&mut a, KeyCode::Enter), Vec::new());
    assert!(a.overlay.is_some(), "the prompt keeps what was typed");
    assert_eq!(a.current_status().unwrap().kind, StatusKind::Warning);
}

#[test]
fn import_offers_the_homes_the_binary_found() {
    let mut a = loaded();
    goto(&mut a, Screen::Profiles);
    assert_eq!(
        press(&mut a, KeyCode::Char('i')),
        vec![Effect::DetectImportSources]
    );
    let _ = a.on_event(Event::Data(Data::ImportSources(vec![PathBuf::from(
        "/home/u/.config/clash-verge-rev",
    )])));
    assert!(matches!(a.overlay, Some(Overlay::Picker { .. })));
    assert_eq!(
        press(&mut a, KeyCode::Enter),
        vec![Effect::ImportProfiles {
            source: PathBuf::from("/home/u/.config/clash-verge-rev")
        }]
    );
}

#[test]
fn a_missing_import_source_is_reported_rather_than_shown_as_an_empty_list() {
    let mut a = loaded();
    let _ = a.on_event(Event::Data(Data::ImportSources(Vec::new())));
    assert!(a.overlay.is_none());
    assert!(
        a.current_status()
            .unwrap()
            .text
            .contains("no clash-verge-rev"),
    );
}

#[test]
fn renaming_offers_the_current_name_and_refuses_an_empty_one() {
    let mut a = loaded();
    goto(&mut a, Screen::Profiles);
    a.profiles
        .select_by_key("patch".to_owned(), |r| r.uid.clone());
    press(&mut a, KeyCode::Char('R'));
    let Some(Overlay::Prompt { value, .. }) = &a.overlay else {
        panic!("a rename prompt");
    };
    assert_eq!(value, "merge", "the prompt starts from the current name");

    // Clearing it and confirming is refused.
    let _ = a.on_event(Event::Key(KeyEvent::new(
        KeyCode::Char('u'),
        KeyModifiers::CONTROL,
    )));
    assert_eq!(press(&mut a, KeyCode::Enter), Vec::new());
    assert_eq!(a.current_status().unwrap().kind, StatusKind::Warning);
}

#[test]
fn the_apply_mode_follows_the_settings() {
    let mut a = loaded();
    a.screen = Screen::Profiles;
    assert_eq!(
        a.dispatch(Action::ApplyConfig, false),
        vec![Effect::ApplyConfig {
            mode: ReloadMode::Auto
        }]
    );
    a.settings.update.prefer_hot_reload = false;
    assert_eq!(
        a.dispatch(Action::ApplyConfig, false),
        vec![Effect::ApplyConfig {
            mode: ReloadMode::Restart
        }],
        "a user who turned hot reload off gets a restart"
    );
}

#[test]
fn a_preview_arrives_as_a_scrollable_overlay() {
    let mut a = loaded();
    a.viewport = (40, 8);
    let _ = a.on_event(Event::Data(Data::Preview(Box::new(Preview {
        summary: "12 proxies, 3 groups, 40 rules - no errors".to_owned(),
        changes: vec![PreviewChange {
            verb: '+',
            path: "proxies[name=JP 02]".to_owned(),
        }],
        findings: vec![PreviewFinding {
            tag: 'W',
            message: "group has no health check".to_owned(),
            location: Some("proxy-groups[0]".to_owned()),
            hint: Some("add url-test".to_owned()),
        }],
        warnings: vec!["a script profile was skipped".to_owned()],
        applicable: true,
        truncated: false,
    }))));
    let Some(Overlay::Preview { lines, scroll, .. }) = &a.overlay else {
        panic!("the preview should be on top");
    };
    assert_eq!(*scroll, 0);
    assert!(lines.iter().any(|l| l.contains("JP 02")));
    assert!(lines.iter().any(|l| l.contains("add url-test")));
    // It scrolls, and Enter closes it.
    let _ = press(&mut a, KeyCode::Down);
    let Some(Overlay::Preview { scroll, .. }) = &a.overlay else {
        panic!("still open");
    };
    assert_eq!(*scroll, 1);
    assert_eq!(press(&mut a, KeyCode::Enter), Vec::new());
    assert!(a.overlay.is_none());
}
