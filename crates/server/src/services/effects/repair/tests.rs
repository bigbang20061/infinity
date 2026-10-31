use super::*;

#[test]
fn shield_repairs_start_and_armor_and_hull_wait_for_end() {
    assert!(applies_at_start(0));
    assert!(!applies_at_start(1));
    assert!(!applies_at_start(2));
}
