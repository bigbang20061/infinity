use super::*;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL and an existing loyalty balance"]
async fn guarded_balance_allows_zero_and_refuses_negative() {
    let config = crate::config::tests::test_database();
    let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.expect("connect");
    tokio::spawn(async move { connection.await.expect("connection"); });
    let tx = client.transaction().await.expect("transaction");
    let row = tx.query_one("SELECT char_id, corp_id, amount FROM game.lp_balances WHERE amount > 0 LIMIT 1", &[]).await.expect("existing positive LP balance");
    let char_id: i64 = row.get("char_id");
    let corp: i64 = row.get("corp_id");
    let before: i64 = row.get("amount");
    assert_eq!(add(&tx, char_id, corp, -before).await.unwrap(), Some(0));
    assert_eq!(add(&tx, char_id, corp, -1).await.unwrap(), None);
    assert_eq!(balance(&tx, char_id, corp).await.unwrap(), Some(0));
    tx.execute("UPDATE game.lp_balances SET amount = -2 WHERE char_id = $1 AND corp_id = $2", &[&char_id, &corp]).await.unwrap();
    assert_eq!(add(&tx, char_id, corp, 1).await.unwrap(), None);
    assert_eq!(balance(&tx, char_id, corp).await.unwrap(), Some(-2));
    assert_eq!(add(&tx, char_id, corp, 2).await.unwrap(), Some(0));
    assert_eq!(balance(&tx, char_id, corp).await.unwrap(), Some(0));
    tx.rollback().await.expect("rollback");
}
