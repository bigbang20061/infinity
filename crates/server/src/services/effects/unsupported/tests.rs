use super::State;

#[test]
fn records_each_effect_and_type_once_and_sorts_snapshot() {
    let mut state = State::default();
    assert!(state.record("unknown_z", 2));
    assert!(state.record("unknown_a", 3));
    assert!(state.record("unknown_a", 1));
    assert!(!state.record("unknown_z", 2));
    assert!(!state.record("unknown_a", 1));
    assert_eq!(
        state.snapshot(),
        vec![("unknown_a".into(), 1), ("unknown_a".into(), 3), ("unknown_z".into(), 2)],
    );
}
