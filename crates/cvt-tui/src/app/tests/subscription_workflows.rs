//! Public key routes and rendered output for subscription maintenance.
use super::*;

#[test]
fn source_and_override_have_distinct_accessible_key_routes() {
    let mut a = loaded();
    goto(&mut a, Screen::Profiles);
    let _ = a.on_event(Event::Data(Data::Profiles(vec![profile(
        "remote",
        "remote",
        Some("https://old.example/sub"),
    )])));
    assert!(press(&mut a, KeyCode::Char('E')).is_empty());
    assert!(matches!(
        a.overlay,
        Some(Overlay::Prompt {
            kind: PromptKind::ProfileUrl,
            ..
        })
    ));
    let _ = press(&mut a, KeyCode::Char('a'));
    let _ = press(&mut a, KeyCode::Esc);
    assert_eq!(
        press(&mut a, KeyCode::Char('o')),
        vec![Effect::EditProfileOverride {
            uid: Some("remote".to_owned())
        }]
    );
    let mut current = profile("remote", "remote", Some("https://old.example/sub"));
    current.current = true;
    let _ = a.on_event(Event::Data(Data::Profiles(vec![current])));
    goto(&mut a, Screen::Proxies);
    assert!(press(&mut a, KeyCode::Char('E')).is_empty());
    assert_eq!(a.pending_profile_source.as_deref(), Some("remote"));
    assert!(matches!(
        a.overlay,
        Some(Overlay::Prompt {
            kind: PromptKind::ProfileUrl,
            ..
        })
    ));
    let _ = press(&mut a, KeyCode::Esc);
    assert_eq!(
        press(&mut a, KeyCode::Char('o')),
        vec![Effect::EditProfileOverride { uid: None }]
    );
    goto(&mut a, Screen::Rules);
    assert_eq!(
        press(&mut a, KeyCode::Char('o')),
        vec![Effect::EditProfileOverride { uid: None }]
    );
    let _ = press(&mut a, KeyCode::Char('a'));
    typed(&mut a, "DOMAIN,new.example,DIRECT");
    assert_eq!(
        press(&mut a, KeyCode::Enter),
        vec![Effect::AddProfileRule {
            rule: "DOMAIN,new.example,DIRECT".to_owned()
        }]
    );
}

#[test]
fn log_keyword_filter_matches_new_arrivals_and_can_be_cleared() {
    let mut a = app();
    goto(&mut a, Screen::Logs);
    for text in ["ordinary connection", "DNS lookup", "dns failure"] {
        let _ = a.on_event(Event::Data(Data::Log(LogRow::new("info", text))));
    }
    let _ = press(&mut a, KeyCode::Char('/'));
    typed(&mut a, "dns");
    let _ = press(&mut a, KeyCode::Enter);
    assert_eq!(a.logs.filtered().len(), 2);
    let _ = a.on_event(Event::Data(Data::Log(LogRow::new(
        "info",
        "new DNS request",
    ))));
    let _ = a.on_event(Event::Data(Data::Log(LogRow::new("info", "unrelated"))));
    assert_eq!(a.logs.filtered().len(), 3);
    let _ = press(&mut a, KeyCode::Esc);
    assert_eq!(a.logs.filtered().len(), 5);
}

#[test]
fn content_change_uses_service_synchronization_even_with_stale_core_status() {
    let mut a = app();
    assert_eq!(
        a.on_event(Event::Done(Done::ProfileContentChanged)),
        vec![Effect::LoadProfiles, Effect::SynchronizeConfig]
    );
    core_running(&mut a);
    assert_eq!(
        a.on_event(Event::Done(Done::ProfileContentChanged)),
        vec![Effect::LoadProfiles, Effect::SynchronizeConfig]
    );
}

#[test]
fn chinese_active_node_status_is_not_clipped_at_normal_terminal_widths() {
    use ratatui::{Terminal, backend::TestBackend};
    let mut a = loaded();
    a.settings.ui.language = Language::Chinese;
    goto(&mut a, Screen::Proxies);
    let mut active = node("active-node", Some("PROXY"), false, Some(12));
    active.active = true;
    let _ = a.on_event(Event::Data(Data::Nodes(vec![
        node("PROXY", None, true, None),
        active,
    ])));
    let _ = press(&mut a, KeyCode::Enter);
    for width in [80, 100, 160] {
        let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
        terminal.draw(|frame| crate::ui::render(frame, &a)).unwrap();
        let buffer = terminal.backend().buffer();
        // Buffer includes a blank continuation cell after each wide glyph.
        let lines = (0..24)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        assert!(
            lines
                .iter()
                .any(|line| line.replace(' ', "").contains("当前使用")),
            "status was clipped at width {width}: {lines:?}"
        );
    }
}

#[test]
fn permission_request_requires_confirmation_and_retries_the_original_operation() {
    let mut a = app();
    let next = Effect::StartCore;
    let binary = PathBuf::from("/isolated/mihomo");
    assert!(
        a.on_event(Event::Data(Data::CoreAuthorization {
            binary: binary.clone(),
            capabilities: "cap_net_bind_service".to_owned(),
            next: Box::new(next.clone()),
        }))
        .is_empty()
    );
    assert!(matches!(
        a.overlay,
        Some(Overlay::Confirm {
            action: Action::AuthorizeCore,
            ..
        })
    ));
    assert_eq!(
        press(&mut a, KeyCode::Char('y')),
        vec![Effect::AuthorizeCore {
            binary,
            capabilities: "cap_net_bind_service".to_owned(),
            next: Box::new(next.clone()),
        }]
    );
    assert_eq!(
        a.on_event(Event::Data(Data::CoreAuthorized {
            next: Box::new(next.clone())
        })),
        vec![next]
    );
}
