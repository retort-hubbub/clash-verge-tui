use super::*;

// -- refresh and ticking ------------------------------------------------

#[test]
fn the_first_visit_to_a_screen_loads_it_and_later_ones_do_not() {
    let mut a = app();
    assert_eq!(
        press(&mut a, KeyCode::Char('2')),
        vec![Effect::LoadProfiles]
    );
    assert_eq!(
        press(&mut a, KeyCode::Char('1')),
        vec![Effect::Refresh(Screen::Home)]
    );
    assert_eq!(press(&mut a, KeyCode::Char('2')), Vec::new());
    assert_eq!(
        press(&mut a, KeyCode::Char('r')),
        vec![Effect::LoadProfiles],
        "an explicit refresh always asks"
    );
    assert_eq!(
        press(&mut a, KeyCode::Char('9')),
        Vec::new(),
        "the help screen has nothing to load"
    );
}

#[test]
fn tabs_move_in_both_directions() {
    let mut a = app();
    assert_eq!(a.screen, Screen::Home);
    let _ = press(&mut a, KeyCode::Tab);
    assert_eq!(a.screen, Screen::Profiles);
    let _ = press(&mut a, KeyCode::BackTab);
    assert_eq!(a.screen, Screen::Home);
}

#[test]
fn a_tick_expires_messages_and_polls_only_a_running_core() {
    let mut a = app();
    for _ in 0..POLL_EVERY {
        assert_eq!(a.on_tick(), Vec::new(), "a stopped core is not polled");
    }
    core_running(&mut a);
    let mut polls = Vec::new();
    for _ in 0..POLL_EVERY {
        polls.extend(a.on_tick());
    }
    assert_eq!(polls, vec![Effect::Refresh(Screen::Home)]);
}

#[test]
fn resizing_updates_how_much_of_a_list_is_on_screen() {
    let mut a = loaded();
    goto(&mut a, Screen::Proxies);
    let _ = a.on_event(Event::Resize(120, 40));
    assert_eq!(a.viewport, (120, 40));
    assert_eq!(a.visible_rows(), 25);
    // A one-row terminal must still be usable rather than a panic.
    let _ = a.on_event(Event::Resize(1, 1));
    assert_eq!(a.visible_rows(), 1);
    assert_eq!(press(&mut a, KeyCode::PageDown), Vec::new());
}

#[test]
fn mouse_selects_tabs_and_visible_rows_without_triggering_actions() {
    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> Event {
        Event::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })
    }
    let mut a = loaded();
    let _ = a.on_event(Event::Resize(120, 40));
    assert_eq!(
        a.on_event(mouse(MouseEventKind::Down(MouseButton::Left), 9, 0)),
        vec![Effect::LoadProfiles]
    );
    assert_eq!(a.screen, Screen::Profiles);
    let area = crate::ui::table_rows_area(&a).unwrap();
    assert!(
        a.on_event(mouse(
            MouseEventKind::Down(MouseButton::Left),
            area.x,
            area.y + 1
        ))
        .is_empty()
    );
    assert_eq!(a.profiles.selected_index(), 1);
    let _ = a.on_event(mouse(MouseEventKind::ScrollUp, area.x, area.y));
    assert_eq!(a.profiles.selected_index(), 0);
    let _ = a.on_event(mouse(
        MouseEventKind::Down(MouseButton::Left),
        area.x,
        area.y + 10,
    ));
    assert_eq!(
        a.profiles.selected_index(),
        0,
        "blank cells do not select a row"
    );

    a.overlay = Some(Overlay::Confirm {
        question: "delete?".to_owned(),
        action: Action::Quit,
    });
    let _ = a.on_event(mouse(MouseEventKind::Down(MouseButton::Left), 2, 0));
    assert_eq!(a.screen, Screen::Profiles, "a modal blocks tab clicks");
    assert!(a.overlay.is_some());
}

#[test]
fn mouse_opens_proxy_groups_and_activates_members_on_double_click() {
    let mut a = loaded();
    goto(&mut a, Screen::Proxies);
    let area = crate::ui::table_rows_area(&a).unwrap();
    let click = |column, row| {
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })
    };
    assert!(!a.is_expanded("PROXY"));
    assert!(a.on_event(click(area.x, area.y)).is_empty());
    assert!(a.is_expanded("PROXY"));
    assert!(a.on_event(click(area.x + 3, area.y + 1)).is_empty());
    assert_eq!(
        a.on_event(click(area.x + 3, area.y + 1)),
        vec![Effect::SelectNode {
            group: "PROXY".to_owned(),
            member: "JP 01".to_owned(),
        }]
    );
    assert!(a.on_event(click(area.x, area.y)).is_empty());
    assert!(!a.is_expanded("PROXY"));
}

#[test]
fn tests_mouse_hitbox_stops_at_the_drawn_list_border() {
    let mut a = loaded();
    goto(&mut a, Screen::Tests);
    let _ = a.on_event(Event::Resize(80, 20));
    let area = crate::ui::table_rows_area(&a).unwrap();
    assert_eq!(area.height, 9);
    let before = a.tests.selected_index();
    let _ = a.on_event(Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: area.x + 2,
        row: area.y + area.height,
        modifiers: KeyModifiers::NONE,
    }));
    assert_eq!(a.tests.selected_index(), before);
}

#[test]
fn mouse_double_click_matches_primary_keyboard_action_across_tables() {
    let click = |column, row| {
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })
    };
    for screen in [
        Screen::Profiles,
        Screen::Rules,
        Screen::Tests,
        Screen::Settings,
    ] {
        let mut a = loaded();
        goto(&mut a, screen);
        let area = crate::ui::table_rows_area(&a).unwrap();
        let _ = a.on_event(click(area.x + 2, area.y));
        let effects = a.on_event(click(area.x + 2, area.y));
        match screen {
            Screen::Profiles => assert!(a.current_status().is_some()),
            Screen::Rules => assert!(matches!(effects.as_slice(), [Effect::ToggleRule { .. }])),
            Screen::Tests => assert!(matches!(effects.as_slice(), [Effect::RunTest { .. }])),
            Screen::Settings => assert!(matches!(a.overlay, Some(Overlay::Prompt { .. }))),
            _ => unreachable!(),
        }
    }
}

#[test]
fn profile_double_click_switches_the_clicked_profile() {
    let mut a = loaded();
    goto(&mut a, Screen::Profiles);
    let _ = a.on_event(Event::Data(Data::Profiles(vec![
        ProfileRow::from_item(&PrfItem::local("base", "Base"), true, false),
        ProfileRow::from_item(&PrfItem::local("other", "Other"), false, false),
    ])));
    let area = crate::ui::table_rows_area(&a).unwrap();
    let click = Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: area.x + 2,
        row: area.y + 1,
        modifiers: KeyModifiers::NONE,
    });
    assert!(a.on_event(click.clone()).is_empty());
    assert_eq!(
        a.on_event(click),
        vec![Effect::SwitchProfile {
            uid: "other".to_owned(),
        }]
    );
}

#[test]
fn settings_description_is_available_in_full_by_key_and_detail_click() {
    let mut a = loaded();
    goto(&mut a, Screen::Settings);
    let help = a.settings_rows.selected_item().unwrap().help.to_owned();
    let _ = press(&mut a, KeyCode::F(1));
    assert!(
        matches!(&a.overlay, Some(Overlay::Preview { lines, .. }) if lines.iter().any(|line| line.contains(&help)))
    );
    let _ = press(&mut a, KeyCode::Esc);
    let area = crate::ui::detail_area(&a).unwrap();
    let _ = a.on_event(Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: area.x + 1,
        row: area.y + 1,
        modifiers: KeyModifiers::NONE,
    }));
    assert!(matches!(a.overlay, Some(Overlay::Preview { .. })));
}

#[test]
fn help_click_selects_and_right_click_opens_that_entry() {
    let mut app = loaded();
    goto(&mut app, Screen::Help);
    let area = crate::ui::table_rows_area(&app).unwrap();
    let row = area.y + 2;
    let click = |button| {
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(button),
            column: area.x + 2,
            row,
            modifiers: KeyModifiers::NONE,
        })
    };
    let _ = app.on_event(click(MouseButton::Left));
    assert_eq!(app.help_selected, 1);
    assert!(app.overlay.is_none());
    let _ = app.on_event(click(MouseButton::Right));
    assert!(matches!(app.overlay, Some(Overlay::Preview { .. })));
}

#[test]
fn help_right_click_in_the_second_column_opens_the_clicked_action() {
    let mut app = loaded();
    goto(&mut app, Screen::Help);
    app.viewport = (160, 40);
    let column = 82;
    let row = 4;
    let expected = crate::ui::help::entry_at(&app, column, row).expect("second column row");
    let _ = app.on_event(Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Right),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }));
    assert_eq!(app.help_selected, expected);
    assert!(matches!(app.overlay, Some(Overlay::Preview { .. })));
}

#[test]
fn mouse_wheel_freezes_and_resumes_the_log_window() {
    let mut a = app();
    a.screen = Screen::Logs;
    for index in 0..8 {
        a.logs.push(LogRow {
            at: "now".to_owned(),
            level: "info".to_owned(),
            message: index.to_string(),
        });
    }
    let wheel = |kind| {
        Event::Mouse(MouseEvent {
            kind,
            column: 2,
            row: 3,
            modifiers: KeyModifiers::NONE,
        })
    };
    let _ = a.on_event(wheel(MouseEventKind::ScrollUp));
    assert!(!a.logs.follow);
    assert_eq!(a.log_window(2).1, 3);
    let _ = a.on_event(wheel(MouseEventKind::ScrollDown));
    assert!(a.logs.follow);
    assert_eq!(a.log_window(2).1, 0);
}

#[test]
fn mouse_click_uses_the_scrolled_rows_and_the_modal_picker() {
    let mut a = app();
    let profiles = (0..40)
        .map(|index| {
            ProfileRow::from_item(
                &PrfItem::local(format!("p{index}"), format!("profile {index}")),
                false,
                false,
            )
        })
        .collect();
    let _ = a.on_event(Event::Data(Data::Profiles(profiles)));
    a.screen = Screen::Profiles;
    let _ = a.on_event(Event::Resize(80, 20));
    a.profiles.select(20);
    a.sync_scroll();
    let offset = a.profiles.offset();
    assert!(offset > 0);
    let area = crate::ui::table_rows_area(&a).unwrap();
    let click = |column, row| {
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })
    };
    let _ = a.on_event(click(area.x, area.y));
    assert_eq!(a.profiles.selected_index(), offset);

    a.overlay = Some(Overlay::Picker {
        title: "new profile".to_owned(),
        items: vec!["from a URL".to_owned(), "a blank local profile".to_owned()],
        selected: 0,
    });
    let picker_row = crate::ui::picker_item_at(a.viewport, 2, 0, 0, 0);
    assert!(picker_row.is_none(), "outside the popup has no choice");
    let popup = crate::ui::widgets::centered(ratatui::layout::Rect::new(0, 0, 80, 20), 72, 4);
    let _ = a.on_event(click(popup.x + 1, popup.y + 2));
    assert!(matches!(
        a.overlay,
        Some(Overlay::Prompt {
            kind: PromptKind::Name,
            ..
        })
    ));
}

#[test]
fn mouse_buttons_confirm_cancel_and_accept_modal_input() {
    let mut a = app();
    a.viewport = (80, 24);
    let click = |column, row| {
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })
    };

    a.overlay = Some(Overlay::Prompt {
        label: "profile name".to_owned(),
        kind: PromptKind::Name,
        value: "sample".to_owned(),
        cursor: 6,
    });
    assert_eq!(
        a.on_event(click(6, 12)),
        vec![Effect::NewProfile {
            name: "sample".to_owned(),
            url: None,
        }]
    );
    a.overlay = Some(Overlay::Prompt {
        label: "profile name".to_owned(),
        kind: PromptKind::Name,
        value: "discard".to_owned(),
        cursor: 7,
    });
    assert!(a.on_event(click(30, 12)).is_empty());
    assert!(a.overlay.is_none());

    a.overlay = Some(Overlay::Confirm {
        question: "quit?".to_owned(),
        action: Action::Quit,
    });
    assert_eq!(
        crate::ui::confirm_choice_at(&a, "quit?", 18, 13),
        Some(false)
    );
    assert!(a.on_event(click(18, 13)).is_empty());
    assert!(a.overlay.is_none());
    assert!(!a.is_quit());
    a.overlay = Some(Overlay::Confirm {
        question: "quit?".to_owned(),
        action: Action::Quit,
    });
    assert_eq!(a.on_event(click(6, 13)), vec![Effect::Quit]);
}

#[test]
fn managed_core_confirmation_has_clickable_buttons_in_both_languages() {
    for language in [Language::English, Language::Chinese] {
        let mut a = app();
        a.settings.ui.language = language;
        a.viewport = (80, 24);
        let question = "download and install the latest managed core?";
        assert!(!a.tr(question).is_empty());
        let (yes_x, yes_y) = (0..24)
            .flat_map(|row| (0..80).map(move |column| (column, row)))
            .find(|&(column, row)| {
                crate::ui::confirm_choice_at(&a, question, column, row) == Some(true)
            })
            .expect("the yes button is clickable");
        a.overlay = Some(Overlay::Confirm {
            question: question.to_owned(),
            action: Action::UpgradeCore,
        });
        assert_eq!(
            a.on_event(Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: yes_x,
                row: yes_y,
                modifiers: KeyModifiers::NONE,
            })),
            vec![Effect::UpgradeCore]
        );
    }
}

#[test]
fn managed_core_action_uses_a_semantic_chinese_confirmation() {
    let mut a = app();
    a.settings.ui.language = Language::Chinese;
    assert!(press(&mut a, KeyCode::Char('U')).is_empty());
    assert!(
        matches!(&a.overlay, Some(Overlay::Confirm { question, .. }) if question == "是否下载并安装最新的托管内核？")
    );
}

#[test]
fn enabling_tun_requires_confirmation_before_saving() {
    let mut app = loaded();
    goto(&mut app, Screen::Settings);
    app.settings_rows
        .select_by_key("core.tun_enabled".to_owned(), |row| row.key.to_owned());
    assert!(app.settings.core.tun_enabled.is_none());
    assert!(press(&mut app, KeyCode::Enter).is_empty());
    assert!(matches!(app.overlay, Some(Overlay::Picker { .. })));
    assert!(app.settings.core.tun_enabled.is_none());
    assert!(press(&mut app, KeyCode::Down).is_empty());
    assert!(press(&mut app, KeyCode::Enter).is_empty());
    assert_eq!(app.settings.core.tun_enabled, Some(true));
    assert!(app.settings_dirty);
    assert!(press(&mut app, KeyCode::Char('s')).is_empty());
    assert!(matches!(app.overlay, Some(Overlay::Confirm { .. })));
}

#[test]
fn footer_click_uses_the_visible_hint_when_no_status_is_displayed() {
    let mut a = app();
    let mut keyboard = app();
    assert_eq!(crate::ui::footer_action_at(&a, 1), Some(Action::StartCore));
    let expected = press(&mut keyboard, KeyCode::Char('s'));
    assert_eq!(
        a.on_event(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 1,
            row: 23,
            modifiers: KeyModifiers::NONE,
        })),
        expected
    );
}

#[test]
fn help_descriptions_can_be_opened_after_scrolling() {
    let mut a = app();
    goto(&mut a, Screen::Help);
    a.viewport = (55, 10);
    for _ in 0..12 {
        let _ = press(&mut a, KeyCode::Down);
    }
    assert_eq!(a.help_selected, 12);
    assert!(a.help_offset > 0);
    let expected = crate::ui::help::entries(&a)[12].description.clone();
    let _ = press(&mut a, KeyCode::Enter);
    assert!(
        matches!(&a.overlay, Some(Overlay::Preview { lines, .. }) if lines.contains(&expected))
    );
}

#[test]
fn full_error_message_survives_footer_expiry_and_scrolls_in_its_popup() {
    let mut a = app();
    a.viewport = (40, 10);
    let message = format!("{}\n{}", "first ".repeat(30), "last detail");
    let _ = a.on_event(Event::Failed(message.clone()));
    let at = a.current_status().unwrap().at;
    a.expire_status_at(at + STATUS_TTL);
    assert!(a.current_status().is_none());
    let _ = a.show_last_message();
    assert!(matches!(&a.overlay, Some(Overlay::Message { text, .. }) if text == &message));
    let _ = a.on_event(Event::Mouse(MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 10,
        row: 5,
        modifiers: KeyModifiers::NONE,
    }));
    assert!(matches!(
        a.overlay,
        Some(Overlay::Message { scroll: 1.., .. })
    ));
    assert!(press(&mut a, KeyCode::Esc).is_empty());
    assert!(a.overlay.is_none());
}

#[test]
fn home_and_tab_summary_open_complete_values() {
    let mut a = app();
    a.viewport = (40, 12);
    a.ip_info = Some(IpInfo {
        ip: "203.0.113.100".to_owned(),
        country: "A long country name".to_owned(),
        organization: "An unusually long network organization".to_owned(),
    });
    let _ = a.on_event(left_click(5, 4));
    assert!(
        matches!(&a.overlay, Some(Overlay::Preview { lines, .. }) if lines.iter().any(|line| line.contains("An unusually long network organization")))
    );
    let _ = press(&mut a, KeyCode::Esc);
    a.viewport = (120, 12);
    let _ = a.on_event(left_click(110, 0));
    assert!(matches!(a.overlay, Some(Overlay::Preview { .. })));
}

#[test]
fn clicking_a_wrapped_log_opens_that_line_in_full() {
    let mut a = app();
    goto(&mut a, Screen::Logs);
    a.viewport = (35, 12);
    let _ = a.on_event(Event::Data(Data::Log(LogRow::new(
        "info",
        "first log line with a long explanation",
    ))));
    let _ = a.on_event(Event::Data(Data::Log(LogRow::new("warn", "second log"))));
    assert!(crate::ui::logs::line_at(&a, 3, 3).is_some_and(|row| row.message.starts_with("first")));
    let _ = a.on_event(left_click(3, 3));
    assert!(
        matches!(&a.overlay, Some(Overlay::Preview { lines, .. }) if lines.join(" ").contains("first log line with a long explanation"))
    );
}
