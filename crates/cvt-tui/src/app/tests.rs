use super::*;
use crate::action::Action;
use crate::row::{TestKind, TestResult};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use cvt_core::profile::item::{PrfItem, ProfileType};

// -- fixtures -----------------------------------------------------------

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn left_click(column: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

/// Send a key press, and collect whatever the machine wants done.
fn press(app: &mut App, code: KeyCode) -> Vec<Effect> {
    app.on_event(Event::Key(key(code)))
}

fn typed(app: &mut App, text: &str) -> Vec<Effect> {
    text.chars()
        .flat_map(|c| press(app, KeyCode::Char(c)))
        .collect()
}

fn app() -> App {
    App::new(PathBuf::from("/tmp/cvt-test-home"), Theme::default())
}

/// Jump to a screen the way the user does, ignoring the first-visit load.
fn goto(app: &mut App, screen: Screen) {
    let digit = char::from(b'1' + u8::try_from(index_of(screen)).unwrap());
    let _ = press(app, KeyCode::Char(digit));
    assert_eq!(app.screen, screen);
}

fn index_of(screen: Screen) -> usize {
    Screen::all().iter().position(|s| *s == screen).unwrap()
}

fn profile(uid: &str, name: &str, url: Option<&str>) -> ProfileRow {
    let item = match url {
        Some(url) => PrfItem::remote(uid, name, url),
        None => PrfItem::local(uid, name),
    };
    ProfileRow::from_item(&item, false, false)
}

fn patch(uid: &str, name: &str, kind: ProfileType) -> ProfileRow {
    ProfileRow::from_item(&PrfItem::patch(uid, name, kind), false, false)
}

fn node(name: &str, group: Option<&str>, is_group: bool, delay: Option<u16>) -> NodeRow {
    NodeRow {
        name: name.to_owned(),
        kind: if is_group { "Selector" } else { "Vless" }.to_owned(),
        group: group.map(str::to_owned),
        delay,
        alive: true,
        active: false,
        is_group,
        is_proxy: !is_group && group.is_some(),
        members: if is_group { 2 } else { 0 },
        selectable: is_group,
    }
}

fn connection(id: &str, destination: &str, upload: u64) -> ConnectionRow {
    ConnectionRow {
        id: id.to_owned(),
        destination: destination.to_owned(),
        network: "tcp".to_owned(),
        process: "curl".to_owned(),
        rule: "MATCH".to_owned(),
        chain: "PROXY".to_owned(),
        upload,
        download: 0,
        started: "10:00:00".to_owned(),
    }
}

fn rule(index: u32, disabled: bool) -> RuleRow {
    RuleRow {
        index,
        kind: "DOMAIN".to_owned(),
        payload: format!("host{index}.test"),
        policy: "PROXY".to_owned(),
        disabled,
        hits: u64::from(index),
        misses: 0,
        raw: format!("DOMAIN,host{index}.test,PROXY"),
    }
}

fn core_running(app: &mut App) {
    let _ = app.on_event(Event::Data(Data::Core(CoreStatus::Running {
        pid: 4321,
        since: 0,
    })));
}

/// An application with a little of everything, and a running core.
fn loaded() -> App {
    let mut a = app();
    core_running(&mut a);
    let _ = a.on_event(Event::Data(Data::Profiles(vec![
        ProfileRow::from_item(&PrfItem::local("base", "base"), true, false),
        patch("patch", "merge", ProfileType::Merge),
    ])));
    let _ = a.on_event(Event::Data(Data::Nodes(vec![
        node("PROXY", None, true, None),
        node("JP 01", Some("PROXY"), false, Some(20)),
        node("US 01", Some("PROXY"), false, Some(300)),
    ])));
    let _ = a.on_event(Event::Data(Data::Connections(vec![
        connection("c1", "a.test:443", 10),
        connection("c2", "b.test:443", 2048),
    ])));
    let _ = a.on_event(Event::Data(Data::Rules(vec![
        rule(0, false),
        rule(1, true),
    ])));
    a
}

mod confirmation_gating;
mod connections;
mod core;
mod filtering;
mod key_routing;
mod logs;
mod navigation;
mod profiles;
mod proxies;
mod refresh_and_ticking;
mod rules;
mod selection_preservation;
mod settings;
mod status;
mod tests_screen;
