use super::*;

// -- connections --------------------------------------------------------

#[test]
fn closing_a_connection_names_it_and_closing_all_asks_first() {
    let mut a = loaded();
    goto(&mut a, Screen::Connections);
    assert_eq!(
        press(&mut a, KeyCode::Char('d')),
        vec![Effect::CloseConnection {
            id: "c1".to_owned()
        }]
    );
    assert_eq!(press(&mut a, KeyCode::Char('D')), Vec::new());
    assert!(matches!(a.overlay, Some(Overlay::Confirm { .. })));
    assert_eq!(
        typed(&mut a, "y"),
        vec![Effect::CloseAllConnections],
        "an explicit yes dispatches the effect"
    );
}

#[test]
fn cycling_the_sort_order_is_local_and_reorders_the_rows() {
    let mut a = loaded();
    goto(&mut a, Screen::Connections);
    assert_eq!(press(&mut a, KeyCode::Char('s')), Vec::new());
    assert_eq!(a.connection_sort, ConnectionSort::Busiest);
    assert_eq!(a.connections.items()[0].id, "c2", "the busiest comes first");
    assert!(a.current_status().unwrap().text.contains("busiest"));
    assert_eq!(press(&mut a, KeyCode::Char('s')), Vec::new());
    assert_eq!(a.connection_sort, ConnectionSort::Oldest);
}
