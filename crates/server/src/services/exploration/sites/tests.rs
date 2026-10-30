//! Focused exploration container and loot selection behavior.
use super::{required_skill_matches, select_loot};

#[test]
fn filters_before_limiting_and_samples_without_replacement() {
    let selected = select_loot(vec![0, 1, 2, 3, 4], 2, |v| *v >= 2, || 0.99);
    assert_eq!(selected, vec![4, 2]);
    assert_eq!(select_loot(vec![0, 1], 5, |v| *v == 1, || 0.0), vec![1]);
    assert!(select_loot(vec![0, 1], 0, |_| true, || 0.0).is_empty());
}

#[test]
fn required_skill_uses_postgres_float_rounding_and_null_semantics() {
    assert!(required_skill_matches(Some(2.5), 2));
    assert!(required_skill_matches(Some(2.6), 3));
    assert!(!required_skill_matches(Some(2.5), 3));
    assert!(!required_skill_matches(None, 0));
}
