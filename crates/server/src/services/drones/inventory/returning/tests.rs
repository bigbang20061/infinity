use super::*;

#[test]
fn scoop_requires_unique_positive_integer_ids_and_sorts_locks() {
    assert!(matches!(parse(&[Value::list([Value::Int(8), Value::Int(3)])]), Ok(ids) if ids == vec![3, 8]));
    for values in [vec![], vec![Value::Int(0)], vec![Value::Int(-1)], vec![Value::Int(3), Value::Int(3)], vec![Value::Bool(true)]] {
        assert!(parse(&[Value::list(values)]).is_err());
    }
    assert!(parse(&[]).is_err());
    assert!(parse(&[Value::Int(3)]).is_err());
}


#[tokio::test]
#[ignore]
async fn concurrent_death_and_scoop_have_one_guarded_winner() {
    let cfg = crate::config::tests::test_database().to_pg_config().unwrap();
    let (mut first, connection) = cfg.connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let (mut second, connection) = cfg.connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let setup = first.transaction().await.unwrap();
    let id = crate::dao::items::creation::positioned(&setup, 1, 101, 103, [1.0, 2.0, 3.0]).await.unwrap();
    setup.commit().await.unwrap();
    let (scooped, dead) = tokio::join!(
        async {
            let tx = first.transaction().await.unwrap();
            let won = drones::return_to_bay(&tx, id, 101, 103, 102, 104).await.unwrap();
            tx.commit().await.unwrap();
            won
        },
        async {
            let tx = second.transaction().await.unwrap();
            let won = drones::destroy(&tx, id, 101, 103).await.unwrap();
            tx.commit().await.unwrap();
            won
        }
    );
    assert_ne!(scooped, dead);
    first.execute("DELETE FROM game.items WHERE item_id = $1", &[&id]).await.unwrap();
}

use crate::services::drones::tests::fixture::Fixture;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn lost_controller_cannot_commit_unclaimed_contact() {
    let f = Fixture::new().await;
    let ty = f.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_DRONE)).unwrap().type_id;
    let id = crate::dao::items::creation::positioned(&f.db, ty, f.owner, f.system, [0.0; 3]).await.unwrap();
    let entries = super::super::restore_specs(&f.db, f.sde.clone(), f.system).await.ok().unwrap();
    let bandwidth = entries[0].1.bandwidth;
    let owner = f.owner; let ship = f.ship;
    let token = f.world.with(f.system, move |s| s.reserve_drone_launch(owner, ship, &[bandwidth], 100.0, 1_000_000.0)).await.unwrap().unwrap();
    let effects = super::super::super::worker::effects(f.db.clone(), Arc::downgrade(&f.world), f.system);
    let ack = f.world.with(f.system, move |s| s.complete_drone_launch(token, owner, ship, entries, effects)).await.unwrap();
    ack.await.unwrap();
    let snapshot = f.world.with(f.system, move |s| s.drone(id).unwrap()).await.unwrap();
    f.world.with(f.system, move |s| s.drone_controller_lost(ship)).await.unwrap();
    contact(&f.db, &f.world, f.system, snapshot, true).await.ok().unwrap();
    let row = f.db.query_one("SELECT location_id, owner_id FROM game.items WHERE item_id=$1", &[&id]).await.unwrap();
    assert_eq!(row.get::<_, i64>("location_id"), f.system);
    assert_eq!(row.get::<_, i64>("owner_id"), owner);
    assert!(f.world.with(f.system, move |s| s.ball(id).is_some() && s.drone(id).is_some_and(|d| d.controller.is_none())).await.unwrap());
    f.cleanup().await;
}