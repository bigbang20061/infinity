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

#[test]
fn capacity_uses_parent_and_matching_children_from_the_same_rows() {
    let mut parent = item(10, 0);
    parent.location_id = 10;
    let mut child = item(11, 3);
    child.location_id = 10;
    child.type_id = 2;
    let mut null_volume = item(12, -2);
    null_volume.location_id = 10;
    null_volume.type_id = 3;
    let unrelated = item(13, 7);
    let rows = [parent, child, null_volume, unrelated];
    let data = |id| match id {
        1 => (Some(20.0), Some(30.0), Some(2.0)),
        2 => (None, None, Some(4.0)),
        _ => (None, None, None),
    };
    assert_eq!(item_capacity(&rows, 10, data), (20.0, 14.0));
    assert_eq!(item_capacity(&rows, 99, data), (0.0, 0.0));
    let empty_children = item_capacity(&rows, 11, |_| (None, Some(9.0), None));
    assert_eq!(empty_children, (9.0, 0.0));
    assert_eq!(empty_children.1.to_bits(), 0.0_f64.to_bits());
    let empty_default = item_capacity(&rows, 11, |_| (None, None, None));
    assert_eq!(empty_default, (0.0, 0.0));
    assert_eq!(empty_default.1.to_bits(), 0.0_f64.to_bits());
    let empty_volume = item_capacity(&rows, 10, |_| (None, None, None));
    assert_eq!(empty_volume.1.to_bits(), 0.0_f64.to_bits());
}
