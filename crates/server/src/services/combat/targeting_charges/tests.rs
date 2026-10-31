use super::*;

#[test]
fn bank_master_resolves_any_group_member() {
    use crate::dao::items::charges::Bank;
    let rows = vec![Bank { master_id: 10, slave_id: 11 }, Bank { master_id: 10, slave_id: 12 }, Bank { master_id: 20, slave_id: 21 }];
    for member in [10, 11, 12] { assert_eq!(bank_master(&rows, member), 10); }
    assert_eq!(bank_master(&rows, 21), 20);
    assert_eq!(bank_master(&rows, 30), 30);
}

#[test]
fn reload_groups_use_each_modules_deadline_and_charge_type() {
    let loads = vec![
        PendingLoad { reload_id: 1, load: (1, 1, 10, 5, 0) },
        PendingLoad { reload_id: 2, load: (2, 2, 10, 3, 0) },
        PendingLoad { reload_id: 3, load: (3, 3, 10, 1, 0) },
        PendingLoad { reload_id: 4, load: (4, 4, 11, 1, 0) },
    ];
    let groups = group_loads(loads, |id| if id == 2 { 2000.0 } else { 1000.0 });
    let ids = |key| groups[&key].iter().map(|(load, _)| load.load.0).collect::<Vec<_>>();
    assert_eq!(ids((filetime_ticks_from_ms(1000.0), 10)), vec![1, 3]);
    assert_eq!(ids((filetime_ticks_from_ms(2000.0), 10)), vec![2]);
    assert_eq!(ids((filetime_ticks_from_ms(1000.0), 11)), vec![4]);
}

#[test]
fn exhausted_stock_only_counts_modules_with_reservations() {
    let item = Item { item_id: 1, type_id: 10, owner_id: 1, location_id: 2, flag_id: FLAG_CARGO,
        quantity: 6, singleton: false, group_id: 0, category_id: 0, custom_info: None };
    let mut items = BTreeMap::from([(1, (item, 6))]);
    let reservations: Vec<_> = (0..3).filter_map(|_| reserve_load(&mut items, &[1], 10, 5, 0)).collect();
    assert_eq!(reservations, vec![5, 1]);
    assert_eq!(reservations.len(), 2);
    assert_eq!(items[&1].1, 0);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with a grouped weapon"]
async fn member_resolution_is_separate_from_master_only_bank() {
    let mut config = crate::config::tests::test_database();
    config.schema_on_start = false;
    let db = Db::connect(&config).await.expect("database pool");
    let ship: i64 = db.query_one("SELECT ship_id FROM game.weapon_banks ORDER BY ship_id LIMIT 1", &[]).await.expect("grouped weapon fixture").get("ship_id");
    let rows = crate::dao::items::charges::banks(&db, ship).await.expect("banks");
    let row = rows.first().expect("grouped weapon fixture");
    let resolved = master_for_member(&db, ship, row.slave_id).await.unwrap_or_else(|_| panic!("member master"));
    assert_eq!(resolved, row.master_id);
    assert_eq!(master_for_member(&db, ship, row.master_id).await.unwrap_or_else(|_| panic!("master")), row.master_id);
    let master = bank(&db, ship, resolved).await.unwrap_or_else(|_| panic!("master bank"));
    assert_eq!(master[0], row.master_id);
    for member in rows.iter().filter(|r| r.master_id == row.master_id) {
        assert!(master.contains(&member.slave_id));
    }
    let mut slave = bank(&db, ship, row.slave_id).await.unwrap_or_else(|_| panic!("slave bank"));
    assert_eq!(slave[0], row.slave_id);
    let mut expected: Vec<_> = std::iter::once(row.slave_id)
        .chain(rows.iter().filter(|r| r.master_id == row.slave_id).map(|r| r.slave_id)).collect();
    slave.sort_unstable();
    expected.sort_unstable();
    assert_eq!(slave, expected);
}
