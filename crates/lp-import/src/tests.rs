//! Database-backed checks of the production corporation transaction.

use super::*;
use serde_json::json;

fn offer(id: i64, quantity: i32) -> offers::Offer {
    offers::Offer {
        offer_id: id, type_id: 10, quantity, lp_cost: 3, isk_cost: 4, ak_cost: 0,
        required_items: json!([{ "type_id": 12, "quantity": 5 }, { "type_id": 11, "quantity": 6 }]),
    }
}

#[tokio::test]
#[ignore = "requires an empty disposable PostgreSQL database in LP_IMPORT_TEST_DATABASE_URL"]
async fn database_upsert_repeatability_and_rollback() {
    let url = std::env::var("LP_IMPORT_TEST_DATABASE_URL").expect("set LP_IMPORT_TEST_DATABASE_URL to an empty disposable database");
    let (mut db, connection) = tokio_postgres::connect(&url, NoTls).await.unwrap();
    let connection_task = tokio::spawn(async move { connection.await.unwrap() });
    // CREATE SCHEMA without IF NOT EXISTS refuses to touch an existing game schema.
    db.batch_execute("CREATE SCHEMA game; CREATE TABLE game.lp_offers (corporation_id bigint NOT NULL, offer_id bigint NOT NULL, type_id integer NOT NULL, quantity integer NOT NULL CHECK (quantity > 0), lp_cost bigint NOT NULL, isk_cost bigint NOT NULL, ak_cost bigint NOT NULL, required_items jsonb NOT NULL, PRIMARY KEY (corporation_id, offer_id));").await.unwrap();
    let initial = [offer(1, 2)];
    store(&mut db, 42, &initial).await.unwrap();
    store(&mut db, 42, &initial).await.unwrap();
    let row = db.query_one("SELECT count(*) AS n, min(quantity) AS quantity, min(required_items::text) AS items FROM game.lp_offers WHERE corporation_id = 42", &[]).await.unwrap();
    assert_eq!(row.get::<_, i64>("n"), 1);
    assert_eq!(row.get::<_, i32>("quantity"), 2);
    let items: serde_json::Value = serde_json::from_str(row.get::<_, &str>("items")).unwrap();
    assert_eq!(items, initial[0].required_items);
    store(&mut db, 42, &[offer(1, 3)]).await.unwrap();
    // The first update must roll back when the second row violates the database constraint.
    assert!(store(&mut db, 42, &[offer(1, 4), offer(2, 0)]).await.is_err());
    let row = db.query_one("SELECT count(*) AS n, min(quantity) AS quantity FROM game.lp_offers WHERE corporation_id = 42", &[]).await.unwrap();
    assert_eq!(row.get::<_, i64>("n"), 1);
    assert_eq!(row.get::<_, i32>("quantity"), 3);
    db.batch_execute("DROP SCHEMA game CASCADE").await.unwrap();
    drop(db);
    connection_task.await.unwrap();
}
