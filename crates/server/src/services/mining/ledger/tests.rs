use super::*;
use crate::{consts::{FILETIME_DAY, FILETIME_SECOND, FILETIME_UNIX_EPOCH}, dao::mining_ledger::{self, Log}};

#[test]
fn wire_rows_round_trip_with_exact_fields() {
    let event_time = FILETIME_UNIX_EPOCH + 20_000 * FILETIME_DAY + 123;
    let quantity = i64::from(i32::MAX) + 37;
    let waste = quantity + 5;
    let log = Log { log_id: 7, character_id: 8, event_time, solar_system_id: 9, type_id: 10, quantity, quantity_wasted: waste };
    let expected = Value::instance("utillib.KeyVal", [
        ("eventDate", Value::Int(event_time)),
        ("solarsystemID", Value::Int(9)),
        ("typeID", Value::Int(10)),
        ("quantity", Value::Int(quantity)),
        ("quantityWasted", Value::Int(waste)),
    ]);
    let rows = Value::list([row(log)]);
    assert_eq!(marshal::loads(&marshal::dumps(&rows)).unwrap(), Value::list([expected]));
    let empty = Value::list([]);
    assert_eq!(marshal::loads(&marshal::dumps(&empty)).unwrap(), empty);
}

#[test]
fn request_accepts_only_authenticated_zero_argument_calls() {
    assert_eq!(validate_request(Some(42), &[], &Value::None).unwrap(), 42);
    assert_eq!(validate_request(Some(42), &[], &Value::Dict(vec![])).unwrap(), 42);
    for character in [None, Some(0), Some(-1)] {
        assert!(validate_request(character, &[], &Value::None).is_err());
    }
    assert!(validate_request(Some(42), &[Value::Int(43)], &Value::None).is_err());
    assert!(validate_request(Some(42), &[], &Value::dict([("characterID", Value::Int(43))])).is_err());
    assert!(validate_request(Some(42), &[], &Value::Int(43)).is_err());
}

#[test]
fn retention_cutoff_is_a_rolling_filetime_window() {
    crate::tests::init_config();
    let now = FILETIME_UNIX_EPOCH + 30_000 * FILETIME_DAY + 123;
    let days = crate::config::game().mining.ledger_retention_days as i64;
    let cutoff = super::super::retention::cutoff(now);
    assert_eq!(now - cutoff, days * FILETIME_DAY);
    assert_eq!(super::super::retention::cutoff(now + 1), cutoff + 1);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn dao_isolation_order_bounds_and_strict_retention() {
    crate::tests::init_config();
    let cfg = crate::config::tests::test_database().to_pg_config().unwrap();
    let (mut client, connection) = cfg.connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let a = -9_001_000_001_i64;
    let b = -9_001_000_002_i64;
    let cutoff = 1_000_i64;
    let large = i64::from(i32::MAX) + 100;
    for (owner, time, qty) in [(a, cutoff - 1, 1), (a, cutoff, large), (a, cutoff, large + 1), (a, cutoff + 1, 3), (b, cutoff - 1, 4), (b, cutoff, 5), (b, cutoff + 1, 6)] {
        mining_ledger::append(&tx, owner, 77, 88, qty, large, time).await.unwrap();
    }
    let rows = mining_ledger::list(&tx, a, cutoff, cutoff + 1).await.unwrap();
    assert_eq!(rows.iter().map(|r| (r.event_time, r.quantity, r.quantity_wasted)).collect::<Vec<_>>(), vec![(cutoff + 1, 3, large), (cutoff, large + 1, large), (cutoff, large, large)]);
    assert!(rows[1].log_id > rows[2].log_id);
    assert_eq!(rows, mining_ledger::list(&tx, a, cutoff, cutoff + 1).await.unwrap());
    assert_eq!(mining_ledger::list(&tx, b, cutoff, cutoff).await.unwrap().iter().map(|r| r.quantity).collect::<Vec<_>>(), vec![5]);
    // The deletion is global; the transaction restores any pre-existing older history too.
    mining_ledger::delete_before(&tx, cutoff).await.unwrap();
    assert_eq!(mining_ledger::list(&tx, a, i64::MIN, i64::MAX).await.unwrap(), rows);
    assert_eq!(mining_ledger::list(&tx, b, i64::MIN, i64::MAX).await.unwrap().iter().map(|r| r.quantity).collect::<Vec<_>>(), vec![6, 5]);
    assert_eq!(mining_ledger::delete_before(&tx, cutoff).await.unwrap(), 0);
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn unbound_rpc_returns_only_retained_owner_logs() -> anyhow::Result<()> {
    use anyhow::{anyhow, ensure, Context};
    use crate::services::drones::tests::fixture::Fixture;

    let mut f = Fixture::new().await;
    let result: anyhow::Result<()> = async {
        let kwargs = Value::None;
        let call = Call { service: "characterMiningLedger", method: "GetCharacterLogs", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        let empty = crate::services::dispatch(&mut f.client, &call).await
            .map_err(|e| anyhow!("empty ledger RPC: {e:?}"))?;
        ensure!(empty == Value::list([]), "new owner ledger was not empty: {empty:?}");

        let (other, _) = f.add_pilot().await;
        let type_id = f.world.ball(f.system, f.ship).await.context("fixture ship missing")?.type_id;
        let now = crate::client::filetime_now();
        let expired = super::super::retention::cutoff(now) - FILETIME_DAY;
        let retained = now - FILETIME_SECOND;
        ensure!(retained > super::super::retention::cutoff(now), "retention window too short for test");
        mining_ledger::append(&f.db, f.owner, f.system, type_id, 1, 2, expired).await?;
        mining_ledger::append(&f.db, f.owner, f.system, type_id, 11, 3, retained).await?;
        mining_ledger::append(&f.db, f.owner, f.system, type_id, 22, 4, retained).await?;
        mining_ledger::append(&f.db, other, f.system, type_id, 33, 5, retained).await?;

        let expected = Value::list([(22, 4), (11, 3)].map(|(quantity, waste)| Value::instance("utillib.KeyVal", [
            ("eventDate", Value::Int(retained)),
            ("solarsystemID", Value::Int(f.system)),
            ("typeID", Value::Int(i64::from(type_id))),
            ("quantity", Value::Int(quantity)),
            ("quantityWasted", Value::Int(waste)),
        ])));
        let stored = mining_ledger::list(&f.db, f.owner, i64::MIN, i64::MAX).await?;
        ensure!(stored.len() == 3 && stored[2].event_time == expired, "expired fact missing from storage: {stored:?}");
        for _ in 0..2 {
            let actual = crate::services::dispatch(&mut f.client, &call).await
                .map_err(|e| anyhow!("ledger RPC: {e:?}"))?;
            let actual = marshal::loads(&marshal::dumps(&actual)).unwrap();
            ensure!(actual == expected, "unexpected ledger response: {actual:?}");
        }
        ensure!(mining_ledger::list(&f.db, f.owner, i64::MIN, i64::MAX).await? == stored, "ledger reads changed stored facts");
        let observers = Call { service: "corpMiningLedger", method: "GetObserversWithMiningEvents", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        ensure!(crate::services::mining::dispatch(&mut f.client, &observers).await.is_none(), "corporate observer service was claimed");
        Ok(())
    }.await;
    f.cleanup().await;
    result
}
