mod automatic;

use super::*;
use crate::services::{Call, drones::tests::fixture::Fixture};

fn dict(entries: Vec<(Value, Value)>) -> Vec<Value> { vec![Value::Dict(entries)] }
fn setting(id: i32, value: bool) -> (Value, Value) { (Value::Int(i64::from(id)), Value::Bool(value)) }

#[test]
fn roundtrip_partial_refusal_and_logout() {
    let owner = -7_001_001;
    logged_off(owner);
    assert_eq!(current(owner), DroneSettings::default());
    let defaults = DroneSettings { aggressive: true, focus_fire: true };
    let full = dict(vec![setting(ATTR_DRONE_IS_AGGRESSIVE, false), setting(ATTR_DRONE_FOCUS_FIRE, true)]);
    let expected = DroneSettings { aggressive: false, focus_fire: true };
    assert_eq!(apply(owner, defaults, parse(&full).ok().unwrap()), expected);
    let partial = dict(vec![setting(ATTR_DRONE_IS_AGGRESSIVE, true)]);
    let expected = DroneSettings { aggressive: true, focus_fire: true };
    assert_eq!(apply(owner, DroneSettings::default(), parse(&partial).ok().unwrap()), expected);
    let bad = dict(vec![setting(ATTR_DRONE_IS_AGGRESSIVE, false), (Value::Int(42), Value::Bool(false))]);
    assert!(matches!(parse(&bad), Err(CallError::User { .. })));
    assert_eq!(current(owner), expected);
    logged_off(owner);
    assert_eq!(current(owner), DroneSettings::default());
    assert_eq!(apply(owner, defaults, Patch { aggressive: Some(false), focus_fire: None }), DroneSettings { aggressive: false, focus_fire: true });
    logged_off(owner);
}

#[test]
fn rejects_bad_shapes_keys_duplicates_and_non_bool_values() {
    let id = ATTR_DRONE_IS_AGGRESSIVE;
    let cases = vec![
        vec![], vec![Value::None], vec![Value::Dict(vec![]), Value::None],
        dict(vec![(Value::str("droneIsAggressive"), Value::Bool(true))]),
        dict(vec![setting(id, true), setting(id, false)]),
        dict(vec![(Value::Int(i64::from(id)), Value::Int(1))]),
        dict(vec![(Value::Bool(true), Value::Bool(true))]),
    ];
    for args in cases { assert!(matches!(parse(&args), Err(CallError::User { .. }))); }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated test database with SDE"]
async fn dogma_dispatch_uses_real_sde_and_session_owner() {
    let mut fixture = Fixture::new().await;
    let owner = fixture.owner;
    let defaults = defaults(&fixture.sde).ok().unwrap();
    let args = dict(vec![setting(ATTR_DRONE_IS_AGGRESSIVE, !defaults.aggressive)]);
    let kwargs = Value::None;
    let call = Call { service: "dogmaIM", method: "ChangeDroneSettings", args: &args, kwargs: &kwargs, bound: true, bind_params: None };
    assert_eq!(crate::services::dogma::dispatch(&mut fixture.client, &call).await.ok().unwrap(), Value::None);
    assert_eq!(current(owner), DroneSettings { aggressive: !defaults.aggressive, focus_fire: defaults.focus_fire });
    let invalid = dict(vec![setting(ATTR_DRONE_FOCUS_FIRE, false), (Value::Int(42), Value::Bool(true))]);
    let call = Call { args: &invalid, ..call };
    assert!(matches!(crate::services::dogma::dispatch(&mut fixture.client, &call).await, Err(CallError::User { .. })));
    assert_eq!(current(owner), DroneSettings { aggressive: !defaults.aggressive, focus_fire: defaults.focus_fire });
    logged_off(owner);
    fixture.cleanup().await;
}
