use super::*;

#[test]
fn extraction_controls_cycle_continuation() {
    assert_eq!(extraction_outcome(true), CycleOutcome::Continue);
    assert_eq!(extraction_outcome(false), CycleOutcome::Stop("mining finished"));
}
