use super::*;

mod booster_db;

#[test]
fn nos_requires_strictly_higher_target_fraction() {
    assert!(!nos_allowed(50.0, 100.0, 100.0, 200.0, false));
    assert!(!nos_allowed(60.0, 100.0, 100.0, 200.0, false));
    assert!(nos_allowed(49.0, 100.0, 100.0, 200.0, false));
    assert!(!nos_allowed(0.0, 0.0, 100.0, 200.0, false));
    assert!(!nos_allowed(0.0, 100.0, 0.0, 0.0, false));
}

#[test]
fn nos_override_bypasses_fraction_rule() {
    assert!(nos_allowed(100.0, 100.0, 1.0, 200.0, true));
    assert!(nos_allowed(50.0, 100.0, 100.0, 200.0, true));
}
