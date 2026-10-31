use super::*;

#[test]
fn environment_preserves_target_and_charge_sublocation() {
    let charge = Value::tuple([Value::Int(30), Value::Int(27), Value::Int(55)]);
    assert_eq!(build(10, 20, 30, Some(40), Some(charge.clone()), 60), Value::list([
        Value::Int(10), Value::Int(20), Value::Int(30), Value::Int(40), charge,
        Value::list([]), Value::Int(60), Value::None,
    ]));
    assert_eq!(build(10, 20, 30, None, None, 60), Value::list([
        Value::Int(10), Value::Int(20), Value::Int(30), Value::None, Value::None,
        Value::list([]), Value::Int(60), Value::None,
    ]));
}
