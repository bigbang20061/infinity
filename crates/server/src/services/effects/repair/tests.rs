use super::*;

#[test]
fn shield_repairs_start_and_armor_and_hull_wait_for_end() {
    assert!(applies_at_start(0));
    assert!(!applies_at_start(1));
    assert!(!applies_at_start(2));
}

#[test]
fn fuel_changes_only_armor_amount_and_uncharged_uses_base() {
    assert_eq!(fueled_amount(100.0, true, Some(2.5)), 250.0);
    assert_eq!(fueled_amount(100.0, false, Some(2.5)), 100.0);
    assert_eq!(fueled_amount(100.0, true, None), 100.0);
}
