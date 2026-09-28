use super::*;

// -- selection preservation ---------------------------------------------

#[test]
fn a_refresh_keeps_the_selected_profile_by_name() {
    let mut a = loaded();
    goto(&mut a, Screen::Profiles);
    a.profiles
        .select_by_key("patch".to_owned(), |r| r.uid.clone());

    // A new subscription arrives at the top of the list.
    let _ = a.on_event(Event::Data(Data::Profiles(vec![
        profile("new", "new", Some("https://new.example/sub")),
        ProfileRow::from_item(&PrfItem::local("base", "base"), true, false),
        patch("patch", "merge", ProfileType::Merge),
    ])));
    assert_eq!(
        a.profiles.selected_item().unwrap().uid,
        "patch",
        "the cursor follows the row, not the position"
    );
}

#[test]
fn a_refresh_keeps_the_selected_node_and_its_expansion() {
    let mut a = loaded();
    goto(&mut a, Screen::Proxies);
    a.expanded.push("PROXY".to_owned());
    let _ = a.on_event(Event::Data(Data::Nodes(vec![
        node("PROXY", None, true, None),
        node("JP 01", Some("PROXY"), false, Some(20)),
        node("US 01", Some("PROXY"), false, Some(300)),
    ])));
    a.nodes
        .select_by_key("US 01".to_owned(), |r| r.name.clone());
    // The same tree arrives again with a fresh latency reading.
    let _ = a.on_event(Event::Data(Data::Nodes(vec![
        node("PROXY", None, true, None),
        node("JP 01", Some("PROXY"), false, Some(15)),
        node("US 01", Some("PROXY"), false, Some(280)),
    ])));
    assert_eq!(a.nodes.selected_item().unwrap().name, "US 01");
    assert_eq!(a.nodes.selected_item().unwrap().delay, Some(280));
    assert_eq!(a.nodes.len(), 3, "the group stays expanded");
}

#[test]
fn a_refresh_keeps_the_selected_connection_by_id() {
    let mut a = loaded();
    goto(&mut a, Screen::Connections);
    a.connections
        .select_by_key("c2".to_owned(), |r| r.id.clone());
    let _ = a.on_event(Event::Data(Data::Connections(vec![connection(
        "c0",
        "new.test:443",
        1,
    )])));
    // The selected connection is gone, so the cursor must simply stay valid.
    assert!(a.connections.selected_item().is_some());
    let _ = a.on_event(Event::Data(Data::Connections(vec![
        connection("c2", "b.test:443", 9999),
        connection("c3", "c.test:443", 1),
    ])));
    assert_eq!(a.connections.selected_item().unwrap().id, "c2");
}
