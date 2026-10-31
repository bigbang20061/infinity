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

#[test]
fn gm_fit_sources_must_belong_to_target_and_be_in_cargo_or_hangar() {
    let mut source = item(10, 2);
    source.owner_id = 42;
    source.location_id = 100;
    source.flag_id = FLAG_CARGO;
    assert!(gm::available(&source, 42, 1, 100, Some(200)));
    assert!(!gm::available(&source, 43, 1, 100, Some(200)));
    source.location_id = 200;
    source.flag_id = FLAG_HANGAR;
    assert!(gm::available(&source, 42, 1, 100, Some(200)));
    assert!(!gm::available(&source, 42, 1, 100, None));
    source.flag_id = FLAG_CARGO;
    assert!(!gm::available(&source, 42, 1, 100, Some(200)));
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL and an existing item"]
async fn stale_move_snapshot_is_refused_without_changing_item() {
    let config = crate::config::tests::test_database();
    let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.expect("connect");
    tokio::spawn(async move { connection.await.expect("connection"); });
    let tx = client.transaction().await.expect("transaction");
    let row = tx.query_one("SELECT item_id, type_id, owner_id, location_id, flag_id, quantity, singleton FROM game.items ORDER BY item_id LIMIT 1 FOR UPDATE", &[]).await.expect("existing item");
    let mut snapshot = Item {
        item_id: row.get("item_id"), type_id: row.get("type_id"), owner_id: row.get("owner_id"),
        location_id: row.get("location_id"), flag_id: row.get("flag_id"), quantity: row.get("quantity"),
        singleton: row.get("singleton"), group_id: 0, category_id: 0, custom_info: None,
    };
    let original_owner = snapshot.owner_id;
    snapshot.owner_id = original_owner.wrapping_add(1);
    assert!(!inventory_dao::move_item(&tx, &snapshot, snapshot.location_id, snapshot.flag_id, snapshot.owner_id, snapshot.singleton, false).await.expect("guarded move"));
    let row = tx.query_one("SELECT owner_id FROM game.items WHERE item_id = $1", &[&snapshot.item_id]).await.expect("item remains");
    assert_eq!(row.get::<_, i64>("owner_id"), original_owner);
    tx.rollback().await.expect("rollback");
}

#[test]
fn remote_item_notifications_route_to_target_not_gm() {
    use crate::db::outbox::Notification;
    let event = Notification::DirectClient { event: "OnItemChange".into(), args: vec![] };
    assert!(matches!(gm::route_target(event, 42, Some(10)), Notification::Pilot { char_id: 42, .. }));
    let event = Notification::DirectClient { event: "OnItemChange".into(), args: vec![] };
    assert!(matches!(gm::route_target(event, 42, Some(42)), Notification::DirectClient { .. }));
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL, loaded SDE and a fitted module with charges"]
async fn charge_unload_and_module_move_roll_back_together() {
    let mut config = crate::config::tests::test_database();
    config.schema_on_start = false;
    let db = crate::db::Db::connect(&config).await.expect("database pool");
    let sde = Sde::load(&db).await.expect("loaded SDE");
    let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.expect("connect");
    tokio::spawn(async move { connection.await.expect("connection"); });
    let tx = client.transaction().await.expect("transaction");
    let row = tx.query_one("SELECT i.item_id, i.owner_id, i.location_id, i.flag_id, c.type_id, c.quantity FROM game.items i JOIN game.module_charges c ON c.ship_id = i.location_id AND c.flag_id = i.flag_id WHERE c.quantity > 0 ORDER BY i.item_id LIMIT 1 FOR UPDATE OF i", &[]).await.expect("fitted module with charge");
    let id: i64 = row.get("item_id");
    let owner: i64 = row.get("owner_id");
    let ship: i64 = row.get("location_id");
    let flag: i32 = row.get("flag_id");
    let charge_type: i32 = row.get("type_id");
    let charge_quantity: i32 = row.get("quantity");
    let module = load_items(&tx, &sde, ItemQuery::Id(id)).await.expect("load module").pop().expect("module");
    let mut outbox: Outbox = Outbox::default();
    assert!(super::super::combat::targeting_charges::unload(&tx, &sde, &mut outbox, owner, ship, &module, ship, FLAG_CARGO).await.is_ok());
    assert!(move_core(&tx, &sde, &mut outbox, &module, owner, ship, FLAG_CARGO, None).await.is_ok());
    tx.rollback().await.expect("rollback");
    let row = client.query_one("SELECT i.owner_id, i.location_id, i.flag_id, c.type_id, c.quantity FROM game.items i JOIN game.module_charges c ON c.ship_id = i.location_id AND c.flag_id = i.flag_id WHERE i.item_id = $1", &[&id]).await.expect("original module and charge");
    assert_eq!((row.get::<_, i64>("owner_id"), row.get::<_, i64>("location_id"), row.get::<_, i32>("flag_id")), (owner, ship, flag));
    assert_eq!((row.get::<_, i32>("type_id"), row.get::<_, i32>("quantity")), (charge_type, charge_quantity));
}

#[tokio::test]
#[ignore]
async fn reserved_shared_move_rolls_back_prior_writes_and_notifications() {
    use std::sync::{Arc, atomic::{AtomicBool, AtomicI64, Ordering}};
    use crate::{config::deployment::DatabaseConfig, dao::{items::transfers, trades}, db::Db};

    let cfg = DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() };
    let db = Db::connect(&cfg).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let type_id = sde.types.iter().next().expect("SDE types required").type_id;
    let written_id = Arc::new(AtomicI64::new(0));
    let dispatched = Arc::new(AtomicBool::new(false));
    let result: Result<(), CallError> = db.transaction(
        {
            let sde = sde.clone();
            let written_id = written_id.clone();
            move |tx, outbox: &mut Outbox| {
                let sde = sde.clone();
                let written_id = written_id.clone();
                Box::pin(async move {
                    let trade = trades::create(tx, 1, 2, 3, 1).await.map_err(db_err)?;
                    let staged = transfers::insert(tx, type_id, 2, trade.id, FLAG_HANGAR, 2).await.map_err(db_err)?;
                    let free = transfers::insert(tx, type_id, 2, 1, FLAG_HANGAR, 2).await.map_err(db_err)?;
                    written_id.store(free, Ordering::SeqCst);
                    let staged_item = load_items(tx, &sde, ItemQuery::Id(staged)).await.map_err(db_err)?.pop().unwrap();
                    let free_item = load_items(tx, &sde, ItemQuery::Id(free)).await.map_err(db_err)?.pop().unwrap();
                    for take in [None, Some(1)] {
                        let mut rejected: Outbox = Outbox::default();
                        assert!(matches!(move_core(tx, &sde, &mut rejected, &staged_item, 2, 1, FLAG_HANGAR, take).await,
                            Err(CallError::User { dict, .. }) if dict == vec![("notify".into(), Value::unicode(messages::trade::TRADE_ITEM_RESERVED))]));
                        assert!(rejected.into_entries().is_empty());
                    }
                    let mut rejected: Outbox = Outbox::default();
                    assert!(matches!(move_core(tx, &sde, &mut rejected, &free_item, 2, trade.id, FLAG_HANGAR, None).await,
                        Err(CallError::User { dict, .. }) if dict == vec![("notify".into(), Value::unicode(messages::trade::TRADE_ITEM_RESERVED))]));
                    assert!(rejected.into_entries().is_empty());
                    move_core(tx, &sde, outbox, &free_item, 2, 3, FLAG_HANGAR, None).await?;
                    move_core(tx, &sde, outbox, &staged_item, 2, 1, FLAG_HANGAR, None).await?;
                    Ok(())
                })
            }
        },
        {
            let dispatched = dispatched.clone();
            move |(), _outbox| async move {
                dispatched.store(true, Ordering::SeqCst);
                Ok(())
            }
        },
    ).await;
    assert!(matches!(result, Err(CallError::User { dict, .. }) if dict == vec![("notify".into(), Value::unicode(messages::trade::TRADE_ITEM_RESERVED))]));
    assert!(!dispatched.load(Ordering::SeqCst));
    assert!(db.query_opt("SELECT item_id FROM game.items WHERE item_id = $1", &[&written_id.load(Ordering::SeqCst)]).await.unwrap().is_none());
}
