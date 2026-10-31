use super::*;
use crate::space::System;

#[test]
fn discarded_concord_callback_clears_marker() {
    crate::tests::init_config();
    let criminal = -101;
    assert!(state().concord.insert(criminal));
    let callback = concord_response(PendingConcord(criminal), 1, Weak::new());
    assert!(state().concord.contains(&criminal));
    drop(callback);
    assert!(!state().concord.contains(&criminal));
}

#[test]
fn dropping_system_clears_queued_concord_marker() {
    crate::tests::init_config();
    let criminal = -102;
    assert!(state().concord.insert(criminal));
    let mut system = System::default();
    system.schedule(1, concord_response(PendingConcord(criminal), 1, Weak::new()));
    assert!(state().concord.contains(&criminal));
    drop(system);
    assert!(!state().concord.contains(&criminal));
}

#[test]
fn executed_concord_callback_allows_another_response() {
    crate::tests::init_config();
    let criminal = -103;
    assert!(state().concord.insert(criminal));
    let mut system = System::default();
    system.schedule(1, concord_response(PendingConcord(criminal), 1, Weak::new()));
    system.tick(1);
    assert!(state().concord.insert(criminal));
    drop(system);
    assert!(state().concord.remove(&criminal));
}

#[test]
fn concord_delay_uses_policy_and_clamps_security() {
    crate::tests::init_config();
    let policy = &crate::config::game().crimewatch;
    assert_eq!(concord_delay_seconds(1.0, policy.concord_base_delay_secs, policy.concord_delay_per_security_secs), 6);
    assert_eq!(concord_delay_seconds(0.9, policy.concord_base_delay_secs, policy.concord_delay_per_security_secs), 8);
    assert_eq!(concord_delay_seconds(0.1, policy.concord_base_delay_secs, policy.concord_delay_per_security_secs), 20);
    assert_eq!(concord_delay_seconds(0.75, 8, 40), 18);
}
