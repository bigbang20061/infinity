//! Public war collection and argument validation behavior.
use super::*;
use crate::dao::wars::State;

fn roundtrip(value: &Value) -> Value { marshal::loads(&marshal::dumps(value)).unwrap() }

#[test]
fn collection_shapes_validate_sort_and_deduplicate() {
    let values = vec![Value::Int(9), Value::Int(8), Value::Int(9)];
    for value in [Value::list(values.clone()), Value::tuple(values.clone()),
        Value::reduce("__builtin__.set", vec![Value::list(values.clone())], None),
        Value::reduce("__builtin__.frozenset", vec![Value::list(values.clone())], None)] {
        assert_eq!(ids(&roundtrip(&value)).unwrap(), vec![8, 9]);
    }
    assert!(ids(&Value::list([Value::Int(0)])).is_err());
    assert!(ids(&Value::Int(8)).is_err());
}

#[test]
fn grouping_includes_both_parties_and_empty_requested_owner() {
    let war = wars::War { war_id: 7, declared_by: 8, against: 9, declared_at: 10, starts_at: 11,
        state: State::Pending, retracted_at: None, retracted_by: None, finishes_at: None };
    let result = roundtrip(&wars_by_owners(&[8, 9, 10], &[war]));
    let Value::Dict(groups) = result else { panic!("expected owner dictionary") };
    assert_eq!(groups.len(), 3);
    for (owner, group) in &groups[..2] {
        assert!(*owner == Value::Int(8) || *owner == Value::Int(9));
        let Value::Dict(wars) = group else { panic!("expected war dictionary") };
        assert_eq!(wars.len(), 1);
        assert_eq!(wars[0].0, Value::Int(7));
        assert_eq!(wars[0].1, wire::war(&wars::War { war_id: 7, declared_by: 8, against: 9, declared_at: 10, starts_at: 11,
            state: State::Pending, retracted_at: None, retracted_by: None, finishes_at: None }));
    }
    assert_eq!(groups[2], (Value::Int(10), Value::Dict(vec![])));
}

#[test]
fn known_methods_require_exact_arity() {
    for method in ["GetWarsByOwnerID", "GetWarsByOwners", "GetTop50", "GetWarsForStructure", "GetPublicWarInfo"] {
        assert!(validate_arity(method, 1).unwrap().is_ok());
        assert!(validate_arity(method, 0).unwrap().is_err());
        assert!(validate_arity(method, 2).unwrap().is_err());
    }
    assert!(validate_arity("GetWarsRequiringAssistance", 0).unwrap().is_ok());
    assert!(validate_arity("GetWarsRequiringAssistance", 1).unwrap().is_err());
    assert!(validate_arity("Unknown", 0).is_none());
}
