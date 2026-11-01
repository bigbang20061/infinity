use super::*;

fn pair(id: i64, count: i64) -> Value { Value::tuple([Value::Int(id), Value::Int(count)]) }

#[test]
fn launch_input_is_exact_and_rejects_duplicate_or_invalid_counts() {
    let valid = [Value::list([pair(10, 2), pair(11, 1)]), Value::Int(99), Value::Bool(false)];
    assert!(matches!(parse(&valid), Ok((rows, Some(99))) if rows == vec![(10, 2), (11, 1)]));
    assert!(matches!(parse(&[Value::list([pair(10, 1)]), Value::None, Value::Bool(false)]), Ok((rows, None)) if rows == vec![(10, 1)]));
    assert!(parse(&[Value::list([pair(10, 1)]), Value::Int(0), Value::Bool(false)]).is_err());
    for rows in [vec![pair(10, 0)], vec![pair(10, -1)], vec![pair(10, 1), pair(10, 1)], vec![Value::Int(10)]] {
        assert!(parse(&[Value::list(rows), Value::Int(99), Value::Bool(false)]).is_err());
    }
    assert!(parse(&[Value::list([pair(10, 1)]), Value::Int(99)]).is_err());
}

#[test]
fn launch_count_gate_uses_effective_character_limit() {
    assert!(check_count(3, 3.0).is_ok());
    assert!(check_count(4, 3.0).is_err());
    assert!(check_count(1, f64::NAN).is_err());
    assert!(check_count(0, 3.0).is_err());
}

#[test]
fn user_failures_keep_named_error_and_dictionary() {
    let value = error_value(crate::services::user_error("Example", vec![("key".into(), Value::Int(4))])).ok().unwrap();
    assert_eq!(value, Value::list([Value::tuple([Value::str("Example"), Value::Dict(vec![(Value::str("key"), Value::Int(4))])])]));
}

use crate::services::drones::tests::fixture::Fixture;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn stale_reservation_and_dropped_ack_keep_one_positioned_drone() {
    let f = Fixture::new().await;
    let ty = f.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_DRONE)).unwrap().type_id;
    let id = crate::dao::items::creation::positioned(&f.db, ty, f.owner, f.system, [0.0; 3]).await.unwrap();
    let entries = super::super::restore_specs(&f.db, f.sde.clone(), f.system).await.ok().unwrap();
    let effects = super::super::super::worker::effects(f.db.clone(), std::sync::Arc::downgrade(&f.world), f.system);
    let owner = f.owner; let ship = f.ship;
    let ack = f.world.with(f.system, move |s| s.complete_drone_launch(0, owner, ship, entries, effects)).await.unwrap();
    drop(ack);
    assert!(f.world.with(f.system, move |s| s.balls().filter(|b| b.id == id).count() == 1 && s.drone(id).is_some_and(|d| d.owner == owner && d.controller.is_none())).await.unwrap());
    assert_eq!(f.db.query_one("SELECT count(*) FROM game.items WHERE item_id=$1 AND owner_id=$2 AND location_id=$3 AND singleton AND x IS NOT NULL", &[&id, &owner, &f.system]).await.unwrap().get::<_, i64>("count"), 1);
    let restored = crate::space::World::new(f.db.clone(), f.sde.clone(), 1);
    restored.system(f.system).await.unwrap();
    assert!(restored.with(f.system, move |s| s.balls().filter(|b| b.id == id).count() == 1 && s.drone(id).is_some_and(|d| d.owner == owner && d.controller.is_none())).await.unwrap());
    f.cleanup().await;
}
