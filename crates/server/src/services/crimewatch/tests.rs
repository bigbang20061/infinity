use super::*;
use crate::space::System;

#[test]
fn discarded_concord_callback_clears_marker() {
    let criminal = -101;
    assert!(state().concord.insert(criminal));
    let callback = concord_response(PendingConcord(criminal), 1, Weak::new());
    assert!(state().concord.contains(&criminal));
    drop(callback);
    assert!(!state().concord.contains(&criminal));
}

#[test]
fn dropping_system_clears_queued_concord_marker() {
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
    let criminal = -103;
    assert!(state().concord.insert(criminal));
    let mut system = System::default();
    system.schedule(1, concord_response(PendingConcord(criminal), 1, Weak::new()));
    system.tick(1);
    assert!(state().concord.insert(criminal));
    drop(system);
    assert!(state().concord.remove(&criminal));
}
