//! Rendering tests.
//!
//! These run against [`TestBackend`], which is a real terminal buffer, so a
//! layout that indexes past its area panics here rather than in the user's
//! terminal. Every screen is drawn empty *and* populated, at four sizes
//! including the degenerate one: a resize passes through one column by one row
//! on its way to any other size, and that frame must not crash the program.

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::Color;

use super::{render, tab_at};
use crate::action::Screen;
use crate::app::{App, Data, Event, Overlay, Preview, PromptKind, StatusKind};
use crate::row::{NodeRow, ProfileRow, RuleRow};
use crate::theme::Theme;

/// The sizes every screen is checked at, smallest first.
const SIZES: [(u16, u16); 4] = [(1, 1), (40, 10), (120, 40), (200, 60)];

fn app_with(theme: Theme) -> App {
    App::new(std::path::PathBuf::from("/tmp/cvt-render-test"), theme)
}

/// Draw once and return everything that ended up on screen.
fn draw(app: &App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test backend");
    terminal.draw(|frame| render(frame, app)).expect("draw");
    text_of(terminal.backend().buffer())
}

/// The buffer as text, one line per row.
fn text_of(buffer: &Buffer) -> String {
    let mut out = String::new();
    for y in 0..buffer.area().height {
        for x in 0..buffer.area().width {
            out.push_str(buffer[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

#[test]
fn clicking_each_rendered_tab_key_opens_its_screen() {
    for language in [
        cvt_core::settings::Language::English,
        cvt_core::settings::Language::Chinese,
    ] {
        for width in [80, 120, 200] {
            let mut app = app_with(Theme::default());
            app.settings.ui.language = language;
            app.viewport = (width, 24);
            let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
            terminal.draw(|frame| render(frame, &app)).unwrap();
            let buffer = terminal.backend().buffer();
            let mut seen = Vec::new();
            for column in 0..width.saturating_sub(2) {
                if buffer[(column, 0)].symbol() == "[" && buffer[(column + 2, 0)].symbol() == "]" {
                    let digit = buffer[(column + 1, 0)].symbol();
                    if let Some(index @ 1..=9) = digit.chars().next().and_then(|ch| ch.to_digit(10))
                    {
                        let screen = Screen::all()[usize::try_from(index - 1).unwrap()];
                        assert_eq!(tab_at(&app, column + 1, 0), Some(screen));
                        seen.push(screen);
                    }
                }
            }
            assert_eq!(seen, Screen::all(), "{language:?} at {width} columns");
        }
    }
}

/// Collapse runs of spaces so a substring can be looked for across cells.
fn flat(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// An application with no data at all, showing `screen`.
fn empty(screen: Screen) -> App {
    let mut app = app_with(Theme::default());
    app.screen = screen;
    app
}

/// An application with rows on every screen, a running core and live samples.
fn ready(screen: Screen) -> App {
    let mut app = populated();
    app.screen = screen;
    app
}

/// A populated application: rows on every screen, a running core, live samples.
fn populated() -> App {
    let mut app = app_with(Theme::default());
    for data in sample_data() {
        let _ = app.on_event(Event::Data(data));
    }
    app
}

fn sample_data() -> Vec<Data> {
    vec![
        Data::Core(cvt_core::mihomo::supervisor::CoreStatus::Running {
            pid: 4321,
            since: 0,
        }),
        Data::Version("v1.19.11".to_owned()),
        Data::Profiles(vec![
            ProfileRow::from_item(
                &cvt_core::profile::item::PrfItem::local("base", "base"),
                true,
                false,
            ),
            ProfileRow::from_item(
                &cvt_core::profile::item::PrfItem::remote(
                    "r1",
                    "Tokyo subscription",
                    "https://sub.example/tokyo",
                ),
                false,
                true,
            ),
            ProfileRow::from_item(
                &cvt_core::profile::item::PrfItem::patch(
                    "m1",
                    "office overrides",
                    cvt_core::profile::item::ProfileType::Merge,
                ),
                false,
                false,
            ),
        ]),
        Data::Nodes(vec![
            NodeRow {
                name: "PROXY".to_owned(),
                kind: "Selector".to_owned(),
                group: None,
                delay: Some(30),
                alive: true,
                active: false,
                is_group: true,
                is_proxy: false,
                members: 2,
                selectable: true,
            },
            NodeRow {
                name: "JP 01".to_owned(),
                kind: "Vless".to_owned(),
                group: Some("PROXY".to_owned()),
                delay: Some(20),
                alive: true,
                active: true,
                is_group: false,
                is_proxy: true,
                members: 0,
                selectable: true,
            },
            NodeRow {
                name: "US 01".to_owned(),
                kind: "Trojan".to_owned(),
                group: Some("PROXY".to_owned()),
                delay: Some(900),
                alive: false,
                active: false,
                is_group: false,
                is_proxy: true,
                members: 0,
                selectable: true,
            },
        ]),
        Data::Connections(vec![
            crate::row::ConnectionRow {
                id: "c1".to_owned(),
                destination: "example.com:443".to_owned(),
                network: "tcp".to_owned(),
                process: "curl".to_owned(),
                rule: "DOMAIN-SUFFIX,example.com".to_owned(),
                chain: "PROXY → JP 01".to_owned(),
                upload: 1024,
                download: 65_536,
                started: "10:00:00".to_owned(),
            },
            crate::row::ConnectionRow {
                id: "c2".to_owned(),
                destination: "1.1.1.1:53".to_owned(),
                network: "udp".to_owned(),
                process: String::new(),
                rule: "MATCH".to_owned(),
                chain: "DIRECT".to_owned(),
                upload: 40,
                download: 80,
                started: "10:00:01".to_owned(),
            },
        ]),
        Data::Rules(vec![
            RuleRow {
                index: 0,
                kind: "DOMAIN-SUFFIX".to_owned(),
                payload: "example.com".to_owned(),
                policy: "PROXY".to_owned(),
                disabled: false,
                hits: 12,
                misses: 30,
                raw: "DOMAIN-SUFFIX,example.com,PROXY".to_owned(),
            },
            RuleRow {
                index: 1,
                kind: "MATCH".to_owned(),
                payload: String::new(),
                policy: "DIRECT".to_owned(),
                disabled: true,
                hits: 0,
                misses: 5_000,
                raw: "MATCH,DIRECT".to_owned(),
            },
        ]),
        Data::RuleProviders(vec!["geosite".to_owned(), "geoip".to_owned()]),
        Data::Traffic(cvt_core::mihomo::types::Traffic {
            up: 2048,
            down: 1_048_576,
            up_total: 10_000,
            down_total: 9_000_000,
        }),
        Data::Memory(52_428_800),
        Data::Log(crate::row::LogRow::new("info", "core is up")),
        Data::Log(crate::row::LogRow::new(
            "error",
            "dial failed for example.com",
        )),
        Data::TestResult {
            mode: crate::row::ProbeMode::Connect,
            kind: crate::row::TestKind::CoreHealth,
            target: "core".to_owned(),
            result: crate::row::TestResult::Passed("v1.19.11".to_owned()),
        },
    ]
}

/// Every action a user can reach, so a screen cannot be forgotten.
fn screens() -> [Screen; 9] {
    Screen::all()
}

#[test]
fn every_screen_renders_empty_at_every_size() {
    for screen in screens() {
        let mut app = empty(screen);
        for (width, height) in SIZES {
            let _ = app.on_event(Event::Resize(width, height));
            let text = draw(&app, width, height);
            assert!(
                !text.trim().is_empty() || height <= 1,
                "{screen} at {width}x{height} drew nothing at all"
            );
        }
    }
}

/// Populated, every screen draws something at every size.
///
/// The name said "renders" and the body asserted only that it did not panic —
/// `let _ = draw(…)`, with the buffer dropped. A screen that drew nothing at
/// all passed, which is the failure this file exists to catch, so it is
/// asserted now the same way the empty case asserts it.
#[test]
fn every_screen_renders_populated_at_every_size() {
    for screen in screens() {
        let mut app = ready(screen);
        for (width, height) in SIZES {
            let _ = app.on_event(Event::Resize(width, height));
            let text = draw(&app, width, height);
            assert!(
                !text.trim().is_empty() || height <= 1,
                "{screen} at {width}x{height} drew nothing with rows on it"
            );
        }
    }
}

#[test]
fn an_empty_screen_explains_itself_rather_than_drawing_a_blank_box() {
    let expectations: [(Screen, &str); 5] = [
        (Screen::Profiles, "no profiles yet"),
        (Screen::Proxies, "no proxies yet"),
        (Screen::Connections, "no connections"),
        (Screen::Rules, "no rules"),
        (Screen::Logs, "no log lines yet"),
    ];
    for (screen, expected) in expectations {
        let app = empty(screen);
        let text = flat(&draw(&app, 120, 40));
        assert!(
            text.contains(expected),
            "{screen} should say {expected:?}, drew: {text}"
        );
    }
}

#[test]
fn the_tab_bar_reports_the_core_state_and_the_live_rate() {
    let app = ready(Screen::Home);
    let text = flat(&draw(&app, 200, 60));
    assert!(
        text.contains("running (pid 4321)"),
        "the core's state has to be visible from every screen: {text}"
    );
    assert!(text.contains('↓'), "the live rate belongs in the tab bar");
    assert!(text.contains("Home") && text.contains("Settings"));
}

#[test]
fn numbered_tabs_are_visible_and_the_digit_has_its_own_colour() {
    let app = ready(Screen::Home);
    let mut terminal = Terminal::new(TestBackend::new(200, 40)).expect("backend");
    terminal.draw(|frame| render(frame, &app)).expect("draw");
    let buffer = terminal.backend().buffer();
    let first_line = text_of(buffer).lines().next().unwrap().to_owned();
    for number in 1..=9 {
        assert!(first_line.contains(&format!("[{number}]")), "{first_line}");
    }
    assert_eq!(buffer[(2, 0)].symbol(), "1");
    assert_ne!(buffer[(2, 0)].fg, buffer[(4, 0)].fg);
    let compact = draw(&app, 80, 24);
    let first_line = compact.lines().next().unwrap();
    for number in 1..=9 {
        assert!(first_line.contains(&format!("[{number}]")), "{first_line}");
    }
}

#[test]
fn chinese_interface_renders_every_screen_at_terminal_sizes() {
    for screen in screens() {
        let mut app = ready(screen);
        app.settings.ui.language = cvt_core::settings::Language::Chinese;
        for (width, height) in SIZES {
            let text = draw(&app, width, height);
            assert!(
                !text.trim().is_empty() || height <= 1,
                "{screen} {width}x{height}"
            );
        }
    }
    let mut app = empty(Screen::Home);
    app.settings.ui.language = cvt_core::settings::Language::Chinese;
    let text = draw(&app, 200, 40);
    let glyphs = text.replace(' ', "");
    assert!(glyphs.contains("[1]首页"), "{text}");
    assert!(glyphs.contains("已停止"), "{text}");
    let narrow = draw(&app, 80, 24);
    let first_line = narrow.lines().next().unwrap();
    for number in 1..=9 {
        assert!(first_line.contains(&format!("[{number}]")), "{first_line}");
    }
}

#[test]
fn a_stopped_core_says_so_in_the_tab_bar() {
    let app = empty(Screen::Home);
    let text = flat(&draw(&app, 200, 60));
    assert!(text.contains("stopped"), "{text}");
}

#[test]
fn a_core_with_no_binary_says_so() {
    let mut app = app_with(Theme::default());
    let _ = app.on_event(Event::Data(Data::Core(
        cvt_core::mihomo::supervisor::CoreStatus::NotInstalled,
    )));
    let text = flat(&draw(&app, 200, 60));
    assert!(text.contains("no core binary"), "{text}");
}

#[test]
fn a_populated_screen_shows_its_rows() {
    let checks: [(Screen, &str); 6] = [
        (Screen::Profiles, "Tokyo subscription"),
        (Screen::Proxies, "PROXY"),
        (Screen::Connections, "example.com:443"),
        (Screen::Rules, "example.com"),
        (Screen::Logs, "dial failed"),
        (Screen::Tests, "core health"),
    ];
    for (screen, expected) in checks {
        let app = ready(screen);
        let text = flat(&draw(&app, 200, 60));
        assert!(
            text.contains(expected),
            "{screen} should show {expected:?}: {text}"
        );
    }
}

#[test]
fn the_proxies_screen_shows_members_only_when_the_group_is_open() {
    let app = ready(Screen::Proxies);
    let collapsed = flat(&draw(&app, 200, 60));
    assert!(collapsed.contains("PROXY"));
    assert!(
        !collapsed.contains("JP 01"),
        "a collapsed group must hide its members: {collapsed}"
    );

    let mut expanded = ready(Screen::Proxies);
    let _ = expanded.on_event(Event::Key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Enter,
        crossterm::event::KeyModifiers::NONE,
    )));
    let open = flat(&draw(&expanded, 200, 60));
    assert!(open.contains("JP 01"), "{open}");
    assert!(open.contains("US 01"), "{open}");
}

#[test]
fn a_disabled_rule_is_hidden_until_it_is_asked_for() {
    let app = ready(Screen::Rules);
    let hidden = flat(&draw(&app, 200, 60));
    assert!(hidden.contains("example.com"));
    assert!(!hidden.contains("MATCH"), "the disabled rule is hidden");
    assert!(hidden.contains("disabled hidden"), "{hidden}");
}

#[test]
fn the_monochrome_theme_leaves_every_cell_uncoloured() {
    for screen in screens() {
        let mut app = ready(screen);
        app.theme = Theme::monochrome();
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).expect("backend");
        terminal.draw(|frame| render(frame, &app)).expect("draw");
        for cell in terminal.backend().buffer().content() {
            assert_eq!(cell.fg, Color::Reset, "{screen}: {cell:?}");
            assert_eq!(cell.bg, Color::Reset, "{screen}: {cell:?}");
        }
    }
}

#[test]
fn the_colour_theme_actually_uses_colour() {
    let app = ready(Screen::Proxies);
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).expect("backend");
    terminal.draw(|frame| render(frame, &app)).expect("draw");
    let coloured = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .any(|cell| cell.fg != Color::Reset);
    assert!(coloured, "the default theme should colour something");
}

#[test]
fn an_empty_state_is_still_drawn_at_a_degenerate_size() {
    // "Is still drawn" was the name and the body discarded every buffer, so it
    // asserted only that a 1x1 or 200x1 terminal does not panic. That is worth
    // knowing and it is not what the name says: where there is room for a
    // character, there has to be one.
    for screen in screens() {
        let app = empty(screen);
        for (width, height) in [(1, 1), (1, 40), (200, 1)] {
            let text = draw(&app, width, height);
            let room = usize::from(width) * usize::from(height);
            if room >= 2 {
                assert!(
                    !text.trim().is_empty(),
                    "{screen} at {width}x{height} drew nothing at all"
                );
            }
        }
    }
}

#[test]
fn overlays_are_drawn_on_top() {
    let mut app = populated();

    // A confirmation.
    app.overlay = Some(Overlay::Confirm {
        question: "stop the core?".to_owned(),
        action: crate::Action::StopCore,
    });
    let text = flat(&draw(&app, 120, 40));
    assert!(text.contains("stop the core?"), "{text}");
    assert!(
        text.contains("[y] yes") && text.contains("[n] no"),
        "{text}"
    );

    // A prompt, with the caret visible.
    app.overlay = Some(Overlay::Prompt {
        label: "filter".to_owned(),
        kind: PromptKind::Search,
        value: "tokyo".to_owned(),
        cursor: 5,
    });
    let text = flat(&draw(&app, 120, 40));
    assert!(text.contains("filter"), "{text}");
    assert!(text.contains("tokyo"), "{text}");

    // A picker.
    app.overlay = Some(Overlay::Picker {
        title: "import from".to_owned(),
        items: vec!["/home/u/.config/clash-verge-rev".to_owned()],
        selected: 0,
    });
    let text = flat(&draw(&app, 120, 40));
    assert!(text.contains("import from"), "{text}");
    assert!(text.contains("clash-verge-rev"), "{text}");

    // The preview.
    app.overlay = Some(Overlay::Preview {
        title: "generated configuration".to_owned(),
        lines: vec![
            "12 proxies, 3 groups, 40 rules".to_owned(),
            "+ proxies[name=JP 02]".to_owned(),
        ],
        scroll: 0,
    });
    let text = flat(&draw(&app, 120, 40));
    assert!(text.contains("generated configuration"), "{text}");
    assert!(text.contains("JP 02"), "{text}");
}

#[test]
fn an_overlay_is_still_safe_at_a_degenerate_size() {
    let mut app = app_with(Theme::default());
    app.overlay = Some(Overlay::Preview {
        title: "generated configuration".to_owned(),
        lines: (0..50).map(|i| format!("line {i}")).collect(),
        scroll: 40,
    });
    for (width, height) in SIZES {
        let _ = draw(&app, width, height);
    }
    app.overlay = Some(Overlay::Picker {
        title: "import from".to_owned(),
        items: Vec::new(),
        selected: 0,
    });
    for (width, height) in SIZES {
        let _ = draw(&app, width, height);
    }
}

#[test]
fn the_status_line_takes_the_footer_and_names_the_problem() {
    let mut app = populated();
    app.screen = Screen::Profiles;
    let _ = app.on_event(Event::Failed(
        "the controller refused the reload".to_owned(),
    ));
    let text = flat(&draw(&app, 200, 60));
    assert!(text.contains("the controller refused the reload"), "{text}");
}

#[test]
fn the_footer_hints_come_from_the_key_map() {
    let app = ready(Screen::Profiles);
    let text = flat(&draw(&app, 200, 60));
    assert!(text.contains("switch"), "the profile keys should be hinted");
    assert!(
        text.contains("edit"),
        "the profile keys should be hinted: {text}"
    );
}

#[test]
fn the_help_screen_is_generated_from_the_key_map() {
    let mut app = populated();
    app.screen = Screen::Help;
    let text = flat(&draw(&app, 200, 60));
    // A group name, an action's label, its description and its key.
    assert!(text.contains("Profiles"), "{text}");
    assert!(text.contains("what it does"), "{text}");
    assert!(text.contains("leave clash-verge-tui"), "{text}");
    assert!(text.contains('q'), "{text}");
}

#[test]
fn a_filtered_list_says_what_it_is_filtered_by() {
    let mut app = ready(Screen::Profiles);
    app.profiles.set_filter("tokyo");
    let text = flat(&draw(&app, 200, 60));
    assert!(text.contains("filter: tokyo"), "{text}");
    assert!(text.contains("Tokyo subscription"));
}

#[test]
fn the_settings_screen_marks_unsaved_changes() {
    let mut app = ready(Screen::Settings);
    let clean = flat(&draw(&app, 200, 60));
    assert!(clean.contains("settings"), "{clean}");
    assert!(!clean.contains("unsaved changes"), "{clean}");
    app.settings_dirty = true;
    let dirty = flat(&draw(&app, 200, 60));
    assert!(dirty.contains("unsaved changes"), "{dirty}");
}

#[test]
fn a_transient_message_does_not_survive_the_next_frame_forever() {
    let mut app = populated();
    let _ = app.on_event(Event::Data(Data::Notice("profiles loaded".to_owned())));
    assert!(flat(&draw(&app, 120, 40)).contains("profiles loaded"));
    // Errors stay; that is the contract the footer relies on.
    let _ = app.on_event(Event::Failed("boom".to_owned()));
    assert!(flat(&draw(&app, 120, 40)).contains("boom"));
}

#[test]
fn the_tests_screen_reports_a_queued_batch() {
    let mut app = ready(Screen::Tests);
    let _ = app.on_event(Event::Key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Enter,
        crossterm::event::KeyModifiers::NONE,
    )));
    assert!(app.queued_tests() > 0);
    let text = flat(&draw(&app, 200, 60));
    assert!(text.contains("queued or running"), "{text}");
    assert!(text.contains("running"), "{text}");
}

#[test]
fn warning_messages_reach_the_screen() {
    let mut app = ready(Screen::Proxies);
    let _ = app.on_event(Event::Data(Data::Notice("a note".to_owned())));
    assert_eq!(app.current_status().map(|s| s.kind), Some(StatusKind::Info));
    assert!(flat(&draw(&app, 200, 60)).contains("a note"));
}

#[test]
fn a_preview_overlay_scrolls_with_its_lines() {
    let mut app = app_with(Theme::default());
    let _ = app.on_event(Event::Data(Data::Preview(Box::new(Preview {
        summary: "12 proxies".to_owned(),
        changes: (0..40)
            .map(|i| crate::app::PreviewChange {
                verb: '+',
                path: format!("proxies[name=node-{i:02}]"),
            })
            .collect(),
        findings: Vec::new(),
        warnings: Vec::new(),
        applicable: true,
        truncated: false,
    }))));
    let first = flat(&draw(&app, 200, 60));
    assert!(first.contains("node-00"), "{first}");
    for _ in 0..10 {
        let _ = app.on_event(Event::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Down,
            crossterm::event::KeyModifiers::NONE,
        )));
    }
    let scrolled = flat(&draw(&app, 200, 60));
    assert_ne!(first, scrolled, "scrolling should move the text");
}
