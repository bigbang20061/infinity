use super::*;
use tokio_postgres::types::Type;

#[test]
fn requirements_preserve_order_and_reject_invalid_data() {
    let decode = |s: &'static str| Requirements::from_sql(&Type::TEXT, s.as_bytes());
    let items = decode(r#"[{"type_id":3,"quantity":2},{"type_id":1,"quantity":4},{"type_id":3,"quantity":5}]"#).unwrap().0;
    assert_eq!(items.iter().map(|x| (x.type_id, x.quantity)).collect::<Vec<_>>(), vec![(3, 2), (1, 4), (3, 5)]);
    for invalid in [r#"{}"#, r#"[{"type_id":1}]"#, r#"[{"type_id":0,"quantity":1}]"#, r#"[{"type_id":1,"quantity":-1}]"#, r#"[{"type_id":2147483648,"quantity":1}]"#] {
        assert!(decode(invalid).is_err(), "accepted {invalid}");
    }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database"]
async fn catalogue_and_lp_owners_are_isolated_and_rollback() {
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    tx.execute("DELETE FROM game.lp_offers", &[]).await.unwrap();
    assert!(is_empty(&tx).await.unwrap());
    tx.execute("INSERT INTO game.lp_offers (corporation_id, offer_id, type_id, quantity, lp_cost, isk_cost, ak_cost, required_items) VALUES (1, 2, 7, 3, 4, 5, 6, '[{\"type_id\":9,\"quantity\":2},{\"type_id\":8,\"quantity\":1},{\"type_id\":9,\"quantity\":3}]'), (1, 1, 7, 1, 0, 0, 0, '[]'), (2, 1, 8, 1, 0, 0, 0, '[]')", &[]).await.unwrap();
    assert!(!is_empty(&tx).await.unwrap());
    let offers = list(&tx, 1).await.unwrap();
    assert_eq!(offers.iter().map(|o| o.offer_id).collect::<Vec<_>>(), vec![1, 2]);
    assert_eq!(get(&tx, 1, 2).await.unwrap(), Some(offers[1].clone()));
    assert_eq!(offers[1].required_items.iter().map(|r| (r.type_id, r.quantity)).collect::<Vec<_>>(), vec![(9, 2), (8, 1), (9, 3)]);
    assert!(get(&tx, 2, 2).await.unwrap().is_none());
    tx.execute("UPDATE game.lp_offers SET required_items = '[{\"type_id\":9,\"quantity\":0}]' WHERE corporation_id = 1 AND offer_id = 2", &[]).await.unwrap();
    assert!(get(&tx, 1, 2).await.is_err());
    assert!(list(&tx, 1).await.is_err());

    let character_owner = 90_000_001_i64;
    let corporation_owner = 98_000_001_i64;
    let issuer = -903_i64;
    tx.execute("DELETE FROM game.lp_balances WHERE char_id = ANY($1) AND corp_id = $2", &[&vec![character_owner, corporation_owner], &issuer]).await.unwrap();
    crate::dao::wallets::loyalty::add(&tx, character_owner, issuer, 10).await.unwrap();
    crate::dao::wallets::loyalty::add(&tx, corporation_owner, issuer, 20).await.unwrap();
    assert_eq!(crate::dao::wallets::loyalty::balance(&tx, character_owner, issuer).await.unwrap(), Some(10));
    assert_eq!(crate::dao::wallets::loyalty::balance(&tx, corporation_owner, issuer).await.unwrap(), Some(20));
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn requirements_only_include_ordered_hangar_stacks_and_guard_changes() {
    use crate::{consts::FLAG_HANGAR, dao::items::{loyalty::requirement_stacks, quantities::{self, QuantityChange}}, db::Db, sde::Sde};
    use std::collections::BTreeMap;
    crate::tests::init_config();
    let cfg = crate::config::tests::test_database();
    let db = Db::connect(&cfg).await.unwrap();
    let sde = Sde::load(&db).await.unwrap();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let mut tx = client.transaction().await.unwrap();
    let owner = -9001_i64;
    let station = -9002_i64;
    let insert = "INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) VALUES ($1, $2, $3, $4, $5, $6) RETURNING item_id";
    let first: i64 = tx.query_one(insert, &[&11_i32, &owner, &station, &FLAG_HANGAR, &5_i32, &false]).await.unwrap().get("item_id");
    let second: i64 = tx.query_one(insert, &[&11_i32, &owner, &station, &FLAG_HANGAR, &7_i32, &false]).await.unwrap().get("item_id");
    for (o, l, f, singleton, ty) in [(owner, station, FLAG_HANGAR, true, 11_i32), (owner, station, FLAG_HANGAR + 1, false, 11), (owner - 1, station, FLAG_HANGAR, false, 11), (owner, station - 1, FLAG_HANGAR, false, 11), (owner, station, FLAG_HANGAR, false, 12)] {
        tx.query_one(insert, &[&ty, &o, &l, &f, &1_i32, &singleton]).await.unwrap();
    }
    let stacks = requirement_stacks(&tx, &sde, owner, station, &[11]).await.unwrap();
    assert_eq!(stacks.iter().map(|item| item.item_id).collect::<Vec<_>>(), vec![first, second]);
    assert!(requirement_stacks(&tx, &sde, owner, station, &[]).await.unwrap().is_empty());
    let changes = BTreeMap::from([(first, QuantityChange::new(&stacks[0], Some(2))), (second, QuantityChange::new(&stacks[1], None))]);
    let inner = tx.transaction().await.unwrap();
    assert!(quantities::apply(&inner, &changes).await.unwrap());
    assert!(!quantities::apply(&inner, &changes).await.unwrap());
    let remaining = requirement_stacks(&inner, &sde, owner, station, &[11]).await.unwrap();
    assert_eq!(remaining.iter().map(|item| (item.item_id, item.quantity)).collect::<Vec<_>>(), vec![(first, 2)]);
    inner.rollback().await.unwrap();
    let restored = requirement_stacks(&tx, &sde, owner, station, &[11]).await.unwrap();
    assert_eq!(restored.iter().map(|item| (item.item_id, item.quantity)).collect::<Vec<_>>(), vec![(first, 5), (second, 7)]);
    tx.rollback().await.unwrap();
}
