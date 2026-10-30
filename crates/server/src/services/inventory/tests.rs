use super::*;

fn item(id: i64, quantity: i32) -> Item {
    Item { item_id: id, type_id: 1, owner_id: 1, location_id: 1, flag_id: 1, quantity, singleton: false, group_id: 1, category_id: 1, custom_info: None }
}

#[test]
fn merge_plan_preserves_order_and_conserves_overlapping_stacks() {
    let items = vec![item(5, 4), item(20, 6), item(30, 3)];
    let plan = plan_merges(&items, &[(20, 30, 4), (5, 20, 3)]).ok().unwrap();
    assert_eq!(plan[&5].quantity, Some(1));
    assert_eq!(plan[&20].quantity, Some(5));
    assert_eq!(plan[&30].quantity, Some(7));
}

#[test]
fn merge_plan_deletes_sources_and_rejects_destination_overflow() {
    let items = vec![item(1, 2), item(2, i32::MAX - 2)];
    let plan = plan_merges(&items, &[(1, 2, 99)]).ok().unwrap();
    assert_eq!(plan[&1].quantity, None);
    assert_eq!(plan[&2].quantity, Some(i32::MAX));
    let overflow = vec![item(1, 2), item(2, i32::MAX - 1)];
    assert!(matches!(plan_merges(&overflow, &[(1, 2, 2)]), Err(CallError::User { .. })));
}
