use super::*;

#[test]
fn cargo_requirements_aggregate_by_station_and_type_with_checked_boundaries() {
    let mut goods = BTreeMap::new();
    assert!(add_requirement(&mut goods, 1, 2, 10).is_ok());
    assert!(add_requirement(&mut goods, 1, 2, 10).is_ok());
    assert!(add_requirement(&mut goods, 2, 2, 10).is_ok());
    assert!(add_requirement(&mut goods, 1, 3, 10).is_ok());
    assert_eq!(goods[&(1, 2)], 20);
    assert_eq!(goods[&(2, 2)], 10);
    assert_eq!(goods[&(1, 3)], 10);
    assert_eq!(missing_amount(goods[&(1, 2)], 10).ok(), Some(10));
    assert_eq!(missing_amount(goods[&(1, 2)], 25).ok(), Some(0));
    assert!(missing_amount(i64::from(i32::MAX) + 1, 0).is_err());
    assert!(add_requirement(&mut goods, 3, 4, i64::MAX).is_ok());
    assert!(add_requirement(&mut goods, 3, 4, 1).is_err());
    assert_eq!(goods[&(3, 4)], i64::MAX);
}
