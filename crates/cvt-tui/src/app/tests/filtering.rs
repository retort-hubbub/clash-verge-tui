use super::*;

// -- filtering ----------------------------------------------------------

#[test]
fn the_search_prompt_filters_as_it_is_typed_and_esc_clears_it() {
    let mut a = loaded();
    goto(&mut a, Screen::Proxies);
    a.expanded.push("PROXY".to_owned());
    let _ = a.on_event(Event::Data(Data::Nodes(vec![
        node("PROXY", None, true, None),
        node("JP 01", Some("PROXY"), false, Some(20)),
        node("US 01", Some("PROXY"), false, Some(300)),
    ])));
    assert_eq!(press(&mut a, KeyCode::Char('/')), Vec::new());
    typed(&mut a, "jp");
    assert_eq!(a.nodes.len(), 1, "typing narrows the list immediately");
    assert_eq!(a.nodes.selected_item().unwrap().name, "JP 01");
    assert_eq!(press(&mut a, KeyCode::Esc), Vec::new());
    assert_eq!(a.nodes.len(), 3, "Esc restores the full list");
    assert_eq!(a.nodes.filter(), "");
}

#[test]
fn search_next_walks_the_matches_and_wraps() {
    let mut a = app();
    let _ = a.on_event(Event::Data(Data::Nodes(vec![
        node("alpha", None, false, None),
        node("beta", None, false, None),
        node("bravo", None, false, None),
    ])));
    goto(&mut a, Screen::Proxies);
    a.nodes.set_filter("b");
    assert_eq!(a.nodes.len(), 2);
    a.nodes.select_first();
    assert_eq!(press(&mut a, KeyCode::Char('n')), Vec::new());
    assert_eq!(a.nodes.selected_item().unwrap().name, "bravo");
    assert_eq!(press(&mut a, KeyCode::Char('n')), Vec::new());
    assert_eq!(
        a.nodes.selected_item().unwrap().name,
        "beta",
        "search next wraps around"
    );
}

#[test]
fn escape_without_a_filter_dismisses_the_footer() {
    let mut a = loaded();
    goto(&mut a, Screen::Rules);
    let _ = a.on_event(Event::Failed("a useful error".to_owned()));
    assert_eq!(press(&mut a, KeyCode::Esc), Vec::new());
    assert!(a.current_status().is_none());
    assert_eq!(a.last_status().unwrap().text, "a useful error");
}
