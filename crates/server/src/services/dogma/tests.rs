use super::*;
use crate::dao::items::creation;

#[path = "../combat/groups_health/tests/fixture.rs"]
mod fixture;
mod capacitor_priming_db;

#[test]
fn item_attribute_union_overlays_and_adds() {
    let mut attrs = BTreeMap::from([(1, 10.0), (2, 20.0)]);
    overlay_overrides(&mut attrs, [(2, 30.0), (3, 40.0)]);
    assert_eq!(attrs, BTreeMap::from([(1, 10.0), (2, 30.0), (3, 40.0)]));
}

#[test]
fn overload_active_effect_has_godma_environment_shape() {
    let (key, value) = active_effect(10, 20, 30, 3001);
    assert_eq!(key, Value::Int(3001));
    let Value::Tuple(fields) = value else { panic!("expected effect tuple") };
    assert_eq!(fields.len(), 10);
    assert_eq!(&fields[..3], &[Value::Int(10), Value::Int(20), Value::Int(30)]);
    assert_eq!(fields[6], Value::Int(3001));
    assert_eq!(fields[8], Value::Int(ONLINE_EFFECT_DURATION));
    assert_eq!(fields[9], Value::Int(ONLINE_EFFECT_REPEAT));
}
