use super::*;

#[test]
fn drone_queries_select_only_the_session_owners_drone() {
    let mut item = Item {
        item_id: 42, type_id: 1, owner_id: 7, location_id: 1, flag_id: 1,
        quantity: 1, singleton: true, group_id: 1,
        category_id: i64::from(CATEGORY_DRONE), custom_info: None,
    };
    assert_eq!(drone_query_key(&item, Action::Query("speed"), Some(7)), Some(Key::Drone(42)));
    assert_eq!(drone_query_key(&item, Action::Query("speed"), Some(8)), None);
    assert_eq!(drone_query_key(&item, Action::Query("speed"), None), None);
    assert_eq!(drone_query_key(&item, Action::Set("speed", 2.0), Some(7)), None);
    assert_eq!(drone_query_key(&item, Action::Reset("speed"), Some(7)), None);
    assert_eq!(drone_query_key(&item, Action::List, Some(7)), None);
    item.category_id = i64::from(CATEGORY_MODULE);
    assert_eq!(drone_query_key(&item, Action::Query("speed"), Some(7)), None);
}
