use super::*;

#[test]
fn ship_effect_start_and_stop_keep_times_and_environment() {
    let charge = Value::tuple([Value::Int(30), Value::Int(27), Value::Int(55)]);
    let env = environment::build(10, 20, 30, Some(40), Some(charge), 60);
    for start in [true, false] {
        let args = ship_effect_args(10, 60, env.clone(), start, 100, 250.0, 3, 200);
        assert_eq!(args, vec![
            Value::Int(10), Value::Int(60), Value::Int(200), Value::Int(start as i64),
            Value::Int(start as i64), env.clone(), Value::Int(100), Value::Float(250.0),
            Value::Int(3), Value::None, if start { Value::None } else { Value::Int(200) },
        ]);
    }
}

#[test]
fn special_fx_has_all_client_arguments() {
    assert_eq!(fire_fx(100, 30, Some((10, 11)), "effects.Laser", Some(40), Some(55), true, true, 250.0, 3),
        event(100, "OnSpecialFX", vec![
            Value::Int(30), Value::Int(10), Value::Int(11), Value::Int(40), Value::Int(55),
            Value::str("effects.Laser"), Value::Bool(true), Value::Int(1), Value::Int(1),
            Value::Float(250.0), Value::Int(3), Value::None, Value::Int(0), Value::None,
        ]));
    assert_eq!(fire_fx(100, 30, None, "effects.Laser", None, None, false, false, 250.0, 0),
        event(100, "OnSpecialFX", vec![
            Value::Int(30), Value::None, Value::None, Value::None, Value::None,
            Value::str("effects.Laser"), Value::Bool(false), Value::Int(0), Value::Int(0),
            Value::Float(250.0), Value::Int(0), Value::None, Value::Int(0), Value::None,
        ]));
}

#[test]
fn attribute_change_preserves_sublocation_and_values() {
    let item = Value::tuple([Value::Int(30), Value::Int(27), Value::Int(55)]);
    let Value::Tuple(args) = attribute_change(20, item.clone(), 99, Value::Int(3), Value::Int(4)) else { panic!("expected attribute change tuple") };
    assert_eq!(args.len(), 8);
    assert_eq!(&args[..4], &[Value::str("OnModuleAttributeChange"), Value::Int(20), item, Value::Int(99)]);
    assert!(matches!(args[4], Value::Int(_)));
    assert_eq!(&args[5..], &[Value::Int(3), Value::Int(4), Value::None]);
}
