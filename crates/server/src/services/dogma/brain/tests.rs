use super::*;

fn target(kind: &'static str, operation: i32, extra: Option<i32>) -> Target {
    Target { ship: true, attribute: 37, kind, operation, extra }
}

#[test]
fn combining_and_wire_envelope() {
    let mut effects = BTreeMap::new();
    let t = target("M", OP_POST_PERCENT, None);
    combine(&mut effects, t.clone(), 10.0, 11);
    combine(&mut effects, t.clone(), 20.0, 11);
    combine(&mut effects, t.clone(), 5.0, 12);
    let e = effects.remove(&t).unwrap();
    assert!((e.value - 38.6).abs() < 1e-9);
    assert_eq!(e.skills, vec![11, 11, 12]);
    let effect = wire(&t, e, 42);
    let Value::Reduce { data, list, dict } = &effect else { panic!("expected ObjectEx reduce") };
    assert!(list.is_empty() && dict.is_empty());
    let Value::Tuple(parts) = data.as_ref() else { panic!("expected reduce tuple") };
    assert!(parts[0] == Value::global("eve.common.script.dogma.effect.BrainEffect"));
    let expected_from = keyval([("invItem", keyval([("typeID", Value::Int(11))]))]);
    assert!(parts[1] == Value::tuple([expected_from, Value::Int(42), Value::str("M"), Value::Int(37), Value::Int(OP_POST_PERCENT as i64), Value::tuple([])]));
    assert!((parts[2].get("value").unwrap().as_float().unwrap() - 38.6).abs() < 1e-9);
    assert!(parts[2].get("fromAttrib") == Some(&Value::None));
    assert!(parts[2].get("toItemID") == Some(&Value::Int(42)));
    assert!(parts[2].get("modifierType") == Some(&Value::str("M")));
    assert!(parts[2].get("toAttribID") == Some(&Value::Int(37)));
    assert!(parts[2].get("operation") == Some(&Value::Int(OP_POST_PERCENT as i64)));
    assert!(parts[2].get("extras") == Some(&Value::tuple([])));
    assert!(parts[2].get("skills") == Some(&Value::list([Value::Int(11), Value::Int(11), Value::Int(12)])));
    let triple = Value::tuple([Value::list([]), Value::list([effect]), Value::list([])]);
    let envelope = Value::tuple([Value::Int(1), Value::Bytes(marshal::dumps(&triple))]);
    let decoded = marshal::loads(&marshal::dumps(&envelope)).unwrap();
    let Value::Tuple(parts) = decoded else { panic!("expected envelope") };
    let Value::Bytes(bytes) = &parts[1] else { panic!("expected inner marshal bytes") };
    assert!(marshal::loads(bytes).unwrap() == triple);
}

#[test]
fn operation_and_target_shapes() {
    for (op, a, b, expected) in [(OP_POST_MUL, 2.0, 3.0, 6.0), (OP_POST_ASSIGN, 2.0, 3.0, 3.0), (OP_MOD_ADD, 2.0, 3.0, 5.0)] {
        let mut effects = BTreeMap::new();
        let t = target("M", op, None);
        combine(&mut effects, t.clone(), a, 1);
        combine(&mut effects, t.clone(), b, 2);
        assert_eq!(effects[&t].value, expected);
    }
    for (kind, extra) in [("M", None), ("L", None), ("LG", Some(5)), ("LRS", Some(6)), ("ORS", Some(7))] {
        let t = target(kind, OP_POST_PERCENT, extra);
        let Value::Reduce { data, .. } = wire(&t, Effect { value: 5.0, skills: vec![1] }, 42) else { panic!("expected reduce") };
        let Value::Tuple(parts) = *data else { panic!("expected tuple") };
        let Value::Tuple(args) = &parts[1] else { panic!("expected args") };
        assert!(args[2] == Value::str(kind));
        assert!(args[5] == Value::tuple(extra.into_iter().map(|v| Value::Int(v as i64))));
    }
}

#[test]
fn hull_override_filter_reset_and_skill_preassignment() {
    let mut effects = BTreeMap::new();
    insert_hull_overrides(&mut effects, &[(37, 12.0), (99, 8.0)], |id| id == 37);
    let t = target("M", OP_PRE_ASSIGN, None);
    assert_eq!(effects.len(), 1);
    let encoded = wire(&t, Effect { value: effects[&t].value, skills: Vec::new() }, 42);
    let Value::Reduce { data, .. } = encoded else { panic!("expected reduce") };
    let Value::Tuple(parts) = *data else { panic!("expected tuple") };
    assert!(parts[2].get("skills") == Some(&Value::list([])));
    assert!(parts[2].get("operation") == Some(&Value::Int(OP_PRE_ASSIGN as i64)));
    assert_eq!(parts[2].get("value").unwrap().as_float(), Some(12.0));
    combine(&mut effects, t.clone(), 7.0, 11);
    assert_eq!(effects[&t].value, 7.0);
    effects.clear();
    insert_hull_overrides(&mut effects, &[], |_| true);
    assert!(effects.is_empty());
}


#[test]
fn booster_bonus_and_penalty_combine_as_unnerfed_percent_on_wire() {
    let mut effects = BTreeMap::new();
    let t = target("LRS", OP_POST_PERCENT_UNNERFED, Some(6));
    combine(&mut effects, t.clone(), 20.0, 101);
    combine(&mut effects, t.clone(), -10.0, 102);
    let effect = effects.remove(&t).unwrap();
    assert!((effect.value - 8.0).abs() < 1e-9);
    let Value::Reduce { data, .. } = wire(&t, effect, 42) else { panic!("expected reduce") };
    let Value::Tuple(parts) = *data else { panic!("expected tuple") };
    assert!(parts[2].get("operation") == Some(&Value::Int(OP_POST_PERCENT_UNNERFED as i64)));
    assert!(parts[2].get("extras") == Some(&Value::tuple([Value::Int(6)])));
    assert!(parts[2].get("skills") == Some(&Value::list([Value::Int(101), Value::Int(102)])));
    assert!((parts[2].get("value").unwrap().as_float().unwrap() - 8.0).abs() < 1e-9);
}

#[test]
fn zero_assignment_and_multiplication_are_emitted_but_zero_division_is_ignored() {
    for op in [OP_PRE_ASSIGN, OP_POST_ASSIGN, OP_PRE_MUL, OP_POST_MUL] {
        let mut effects = BTreeMap::new();
        let t = target("M", op, None);
        combine(&mut effects, t.clone(), 0.0, 101);
        let Value::Reduce { data, .. } = wire(&t, effects.remove(&t).unwrap(), 42) else { panic!("expected reduce") };
        let Value::Tuple(parts) = *data else { panic!("expected tuple") };
        assert_eq!(parts[2].get("value").unwrap().as_float(), Some(0.0));
        assert!(parts[2].get("operation") == Some(&Value::Int(op as i64)));
    }
    for op in [OP_PRE_DIV, OP_POST_DIV, OP_MOD_ADD, OP_MOD_SUB, OP_POST_PERCENT, OP_POST_PERCENT_UNNERFED] {
        let mut effects = BTreeMap::new();
        combine(&mut effects, target("M", op, None), 0.0, 101);
        assert!(effects.is_empty());
    }
}

