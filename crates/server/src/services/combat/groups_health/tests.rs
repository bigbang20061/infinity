use super::*;
use crate::dao::items::creation;

mod db_smoke;
mod fixture;
#[test]
fn recharge_uses_existing_curve_and_never_runs_backwards() {
    let level = recharge(25.0, 100.0, 1000.0, 1.0);
    assert!((level - 100.0 * (1.0 - 0.5 * (-5.0_f64).exp()).powi(2)).abs() < 1e-9);
    assert_eq!(recharge(25.0, 100.0, 1000.0, -1.0), 25.0);
}

#[test]
fn injection_and_capacity_are_clamped() {
    let mut entries = HashMap::new();
    entries.insert(1, (80.0, 0));
    let result = mutate(&mut entries, 1, 100.0, 1000.0, 0, Operation::Inject(50.0));
    assert_eq!((result.after, result.amount), (100.0, 20.0));
    let result = mutate(&mut entries, 1, 50.0, 1000.0, 0, Operation::Inject(-5.0));
    assert_eq!((result.after, result.amount), (50.0, 0.0));
    let result = mutate(&mut entries, 1, -1.0, 1000.0, 0, Operation::Snapshot);
    assert_eq!(result.after, 0.0);
}

#[test]
fn drain_is_partial_and_negative_input_cannot_add_energy() {
    let mut entries = HashMap::new();
    entries.insert(1, (30.0, 0));
    let result = mutate(&mut entries, 1, 100.0, 1000.0, 0, Operation::Drain(50.0));
    assert_eq!(result.after, 0.0);
    // The recharge curve's sqrt/square round trip can introduce rounding at dt = 0.
    assert!((result.amount - 30.0).abs() < 1e-12);
    let result = mutate(&mut entries, 1, 100.0, 1000.0, 0, Operation::Drain(-10.0));
    assert_eq!((result.after, result.amount), (0.0, 0.0));
}

#[test]
fn refused_cycle_drain_leaves_stored_state_untouched() {
    let mut entries = HashMap::new();
    entries.insert(1, (20.0, 0));
    let result = mutate(&mut entries, 1, 100.0, 1000.0, 0, Operation::RefuseDrain(30.0));
    assert_eq!(result.amount, 0.0);
    assert_eq!(entries.get(&1), Some(&(20.0, 0)));
}
