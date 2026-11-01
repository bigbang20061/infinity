//! Dividend conservation, replay and master-wallet authorization.
use super::fixtures;
use crate::{consts::ACCOUNT_CASH, dao::corporations::shares as ledger, db::outbox::Outbox, services::corps::dividends::payout_core};

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn mixed_holders_receive_floored_amounts_from_master_only() {
    let mut client = fixtures::connect().await;
    let tx = client.transaction().await.unwrap();
    let ceo = fixtures::character(&tx).await;
    let person = fixtures::character(&tx).await;
    let other_ceo = fixtures::character(&tx).await;
    let issuer = fixtures::corporation(&tx, ceo).await;
    let other = fixtures::corporation(&tx, other_ceo).await;
    assert!(ledger::initialize(&tx, issuer, 1000).await.unwrap());
    assert!(ledger::transfer(&tx, issuer, issuer, person, 1).await.unwrap());
    assert!(ledger::transfer(&tx, issuer, issuer, other, 2).await.unwrap());
    tx.execute("UPDATE game.characters SET balance=0 WHERE character_id=$1", &[&person]).await.unwrap();
    tx.execute("INSERT INTO game.corp_wallets (corporation_id,account_key,balance) VALUES ($1,$3,10),($1,$4,100),($2,$3,0)", &[&issuer, &other, &ACCOUNT_CASH, &(ACCOUNT_CASH + 1)]).await.unwrap();
    let op = ledger::operation_id(&tx).await.unwrap();
    let mut effects = Outbox::<()>::default();
    payout_core(&tx, &mut effects, op, ceo, issuer, 1.0, 1).await.unwrap();
    let row = tx.query_one("SELECT (SELECT balance::float8 FROM game.corp_wallets WHERE corporation_id=$1 AND account_key=$4) AS master, (SELECT balance::float8 FROM game.corp_wallets WHERE corporation_id=$1 AND account_key=$5) AS division, (SELECT balance::float8 FROM game.corp_wallets WHERE corporation_id=$2 AND account_key=$4) AS corporate, (SELECT balance::float8 FROM game.characters WHERE character_id=$3) AS personal, (SELECT distributed::float8 FROM game.corp_dividends WHERE operation_id=$6) AS distributed", &[&issuer, &other, &person, &ACCOUNT_CASH, &(ACCOUNT_CASH + 1), &op]).await.unwrap();
    assert_eq!(row.get::<_, f64>("master"), 9.01);
    assert_eq!(row.get::<_, f64>("division"), 100.0);
    assert_eq!(row.get::<_, f64>("corporate"), 0.66);
    assert_eq!(row.get::<_, f64>("personal"), 0.33);
    assert_eq!(row.get::<_, f64>("distributed"), 0.99);
    let receipt = tx.query_one("SELECT count(*) AS count, sum(shares)::bigint AS shares, sum(amount)::text AS total FROM game.corp_dividend_recipients WHERE operation_id=$1", &[&op]).await.unwrap();
    assert_eq!(receipt.get::<_, i64>("count"), 2);
    assert_eq!(receipt.get::<_, i64>("shares"), 3);
    assert_eq!(receipt.get::<_, String>("total"), "0.99");
    assert!(!effects.into_entries().is_empty());
    tx.execute("UPDATE game.corporations SET ceo_id=$2 WHERE corporation_id=$1", &[&issuer, &person]).await.unwrap();
    tx.execute("UPDATE game.characters SET corporation_id=1 WHERE character_id=$1", &[&ceo]).await.unwrap();
    let mut replay = Outbox::<()>::default();
    payout_core(&tx, &mut replay, op, ceo, -1, f64::NAN, 1).await.unwrap();
    assert!(replay.into_entries().is_empty());
    assert_eq!(tx.query_one("SELECT count(*) AS count FROM game.corp_dividends WHERE operation_id=$1", &[&op]).await.unwrap().get::<_, i64>("count"), 1);
    let after = tx.query_one("SELECT (SELECT balance::float8 FROM game.corp_wallets WHERE corporation_id=$1 AND account_key=$4) AS master, (SELECT balance::float8 FROM game.corp_wallets WHERE corporation_id=$2 AND account_key=$4) AS corporate, (SELECT balance::float8 FROM game.characters WHERE character_id=$3) AS personal", &[&issuer, &other, &person, &ACCOUNT_CASH]).await.unwrap();
    assert_eq!(after.get::<_, f64>("master"), 9.01);
    assert_eq!(after.get::<_, f64>("corporate"), 0.66);
    assert_eq!(after.get::<_, f64>("personal"), 0.33);
    tx.execute("UPDATE game.characters SET corporation_id=$1 WHERE character_id=$2", &[&issuer, &ceo]).await.unwrap();
    tx.execute("UPDATE game.corporations SET ceo_id=$2 WHERE corporation_id=$1", &[&issuer, &ceo]).await.unwrap();
    for amount in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.0, -1.0] {
        let mut effects = Outbox::<()>::default();
        assert!(payout_core(&tx, &mut effects, ledger::operation_id(&tx).await.unwrap(), ceo, issuer, amount, 1).await.is_err());
        assert!(effects.into_entries().is_empty());
    }
    let mut effects = Outbox::<()>::default();
    assert!(payout_core(&tx, &mut effects, ledger::operation_id(&tx).await.unwrap(), person, issuer, 1.0, 1).await.is_err());
    assert!(effects.into_entries().is_empty());
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn missing_recipients_insufficient_master_and_unsafe_amount_roll_back() {
    let mut client = fixtures::connect().await;
    let mut tx = client.transaction().await.unwrap();
    let ceo = fixtures::character(&tx).await;
    let holder = fixtures::character(&tx).await;
    let issuer = fixtures::corporation(&tx, ceo).await;
    assert!(ledger::initialize(&tx, issuer, 1000).await.unwrap());
    tx.execute("INSERT INTO game.corp_wallets (corporation_id,account_key,balance) VALUES ($1,$2,0),($1,$3,1000000)", &[&issuer, &ACCOUNT_CASH, &(ACCOUNT_CASH + 1)]).await.unwrap();
    let op = ledger::operation_id(&tx).await.unwrap();
    let savepoint = tx.transaction().await.unwrap();
    let mut effects = Outbox::<()>::default();
    assert!(payout_core(&savepoint, &mut effects, op, ceo, issuer, 10.0, 1).await.is_err());
    assert!(effects.into_entries().is_empty());
    savepoint.rollback().await.unwrap();
    assert!(ledger::transfer(&tx, issuer, issuer, holder, 1).await.unwrap());
    for amount in [10.0, 1e35] {
        let op = ledger::operation_id(&tx).await.unwrap();
        let savepoint = tx.transaction().await.unwrap();
        let mut effects = Outbox::<()>::default();
        assert!(payout_core(&savepoint, &mut effects, op, ceo, issuer, amount, 1).await.is_err());
        assert!(effects.into_entries().is_empty());
        savepoint.rollback().await.unwrap();
        let count: i64 = tx.query_one("SELECT count(*) AS count FROM game.corp_dividends WHERE operation_id=$1", &[&op]).await.unwrap().get("count");
        assert_eq!(count, 0);
    }
    let division: f64 = tx.query_one("SELECT balance::float8 AS balance FROM game.corp_wallets WHERE corporation_id=$1 AND account_key=$2", &[&issuer, &(ACCOUNT_CASH + 1)]).await.unwrap().get("balance");
    assert_eq!(division, 1000000.0);
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn numeric_allocations_with_inexact_float_roundtrip_are_refused() {
    use crate::dao::corporations::dividends;
    let mut client = fixtures::connect().await;
    let tx = client.transaction().await.unwrap();
    let ceo = fixtures::character(&tx).await;
    let first = fixtures::character(&tx).await;
    let second = fixtures::character(&tx).await;
    let issuer = fixtures::corporation(&tx, ceo).await;
    assert!(ledger::initialize(&tx, issuer, 1000).await.unwrap());
    assert!(ledger::transfer(&tx, issuer, issuer, first, 1).await.unwrap());
    assert!(ledger::transfer(&tx, issuer, issuer, second, 2).await.unwrap());
    let op = ledger::operation_id(&tx).await.unwrap();
    let amount = 100_000_000_000_000.0;
    assert!(dividends::claim(&tx, op, issuer, ACCOUNT_CASH, amount, ceo, 1).await.unwrap());
    assert!(dividends::allocate(&tx, op, issuer, amount).await.unwrap().is_none());
    let count: i64 = tx.query_one("SELECT count(*) AS count FROM game.corp_dividend_recipients WHERE operation_id=$1", &[&op]).await.unwrap().get("count");
    assert_eq!(count, 0);
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn zero_cent_recipient_is_recorded_without_spending_residue() {
    let mut client = fixtures::connect().await;
    let tx = client.transaction().await.unwrap();
    let ceo = fixtures::character(&tx).await;
    let small = fixtures::character(&tx).await;
    let large = fixtures::character(&tx).await;
    let issuer = fixtures::corporation(&tx, ceo).await;
    assert!(ledger::initialize(&tx, issuer, 1000).await.unwrap());
    assert!(ledger::transfer(&tx, issuer, issuer, small, 1).await.unwrap());
    assert!(ledger::transfer(&tx, issuer, issuer, large, 999).await.unwrap());
    tx.execute("INSERT INTO game.corp_wallets (corporation_id,account_key,balance) VALUES ($1,$2,2)", &[&issuer, &ACCOUNT_CASH]).await.unwrap();
    let op = ledger::operation_id(&tx).await.unwrap();
    let mut effects = Outbox::<()>::default();
    payout_core(&tx, &mut effects, op, ceo, issuer, 1.0, 1).await.unwrap();
    let rows = tx.query("SELECT holder_id,amount::text AS amount FROM game.corp_dividend_recipients WHERE operation_id=$1 ORDER BY holder_id", &[&op]).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows.iter().find(|r| r.get::<_, i64>("holder_id") == small).unwrap().get::<_, String>("amount"), "0.00");
    assert_eq!(rows.iter().find(|r| r.get::<_, i64>("holder_id") == large).unwrap().get::<_, String>("amount"), "0.99");
    let master: f64 = tx.query_one("SELECT balance::float8 AS balance FROM game.corp_wallets WHERE corporation_id=$1 AND account_key=$2", &[&issuer, &ACCOUNT_CASH]).await.unwrap().get("balance");
    assert_eq!(master, 1.01);
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn fractional_request_is_floored_per_holder_without_prerounding() {
    let mut client = fixtures::connect().await;
    let tx = client.transaction().await.unwrap();
    let ceo = fixtures::character(&tx).await;
    let first = fixtures::character(&tx).await;
    let second = fixtures::character(&tx).await;
    let issuer = fixtures::corporation(&tx, ceo).await;
    assert!(ledger::initialize(&tx, issuer, 2).await.unwrap());
    assert!(ledger::transfer(&tx, issuer, issuer, first, 1).await.unwrap());
    assert!(ledger::transfer(&tx, issuer, issuer, second, 1).await.unwrap());
    tx.execute("INSERT INTO game.corp_wallets (corporation_id,account_key,balance) VALUES ($1,$2,2)", &[&issuer, &ACCOUNT_CASH]).await.unwrap();
    let op = ledger::operation_id(&tx).await.unwrap();
    let mut effects = Outbox::<()>::default();
    payout_core(&tx, &mut effects, op, ceo, issuer, 0.015, 1).await.unwrap();
    assert!(effects.into_entries().is_empty());
    let row = tx.query_one("SELECT requested::float8 AS requested, distributed::float8 AS distributed, (SELECT balance::float8 FROM game.corp_wallets WHERE corporation_id=$2 AND account_key=$3) AS master, (SELECT count(*) FROM game.corp_dividend_recipients WHERE operation_id=$1 AND amount=0) AS zero_count FROM game.corp_dividends WHERE operation_id=$1", &[&op, &issuer, &ACCOUNT_CASH]).await.unwrap();
    assert_eq!(row.get::<_, f64>("requested"), 0.015);
    assert_eq!(row.get::<_, f64>("distributed"), 0.0);
    assert_eq!(row.get::<_, f64>("master"), 2.0);
    assert_eq!(row.get::<_, i64>("zero_count"), 2);
    assert!(ledger::transfer(&tx, issuer, second, first, 1).await.unwrap());
    let op = ledger::operation_id(&tx).await.unwrap();
    let mut effects = Outbox::<()>::default();
    payout_core(&tx, &mut effects, op, ceo, issuer, 0.015, 1).await.unwrap();
    let row = tx.query_one("SELECT requested::float8 AS requested, distributed::float8 AS distributed, (SELECT balance::float8 FROM game.corp_wallets WHERE corporation_id=$2 AND account_key=$3) AS master FROM game.corp_dividends WHERE operation_id=$1", &[&op, &issuer, &ACCOUNT_CASH]).await.unwrap();
    assert_eq!(row.get::<_, f64>("requested"), 0.015);
    assert_eq!(row.get::<_, f64>("distributed"), 0.01);
    assert_eq!(row.get::<_, f64>("master"), 1.99);
    tx.rollback().await.unwrap();
}
