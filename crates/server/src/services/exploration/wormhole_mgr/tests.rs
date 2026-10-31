use super::*;
use std::sync::atomic::AtomicBool;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL and exploration schema"]
async fn later_endpoint_failure_rolls_back_entire_batch_without_dispatch() {
    let mut config = crate::config::tests::test_database();
    config.schema_on_start = false;
    let db = Db::connect(&config).await.expect("database pool");
    let system = i64::MIN;
    assert!(crate::dao::exploration::sites(&db, system).await.unwrap().is_empty());
    let mut taken = HashSet::new();
    let mut site = || new_site(system, [0.0; 3], Kind::Signature, STRENGTH_WORMHOLE, WORMHOLE_SCAN_EASE, &mut taken);
    let mut a = site();
    let mut b = site();
    let mut c = site();
    let mut d = site();
    a.id = i64::MIN;
    b.id = i64::MIN + 1;
    c.id = i64::MIN + 2;
    d.id = i64::MIN + 3;
    d.target = "invalid\0target".into();
    let pairs = vec![(a, b), (c, d)];
    let dispatched = AtomicBool::new(false);
    let result: Result<(), CallError> = db.transaction(
        |tx, outbox: &mut Outbox<Vec<(Site, Site)>>| {
            let pairs = pairs.clone();
            Box::pin(async move { save_pairs(tx, outbox, pairs).await })
        },
        |(), _: Outbox<Vec<(Site, Site)>>| async {
            dispatched.store(true, Ordering::Relaxed);
            Ok(())
        },
    ).await;
    assert!(result.is_err());
    assert!(!dispatched.load(Ordering::Relaxed));
    assert!(crate::dao::exploration::sites(&db, system).await.unwrap().is_empty());
}
