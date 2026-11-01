use super::*;

#[test]
fn amount_respects_falloff_and_invalid_input() {
    assert_eq!(scaled_amount(100.0, 10.0, 10.0, 20.0), 100.0);
    assert_eq!(scaled_amount(100.0, 30.0, 10.0, 20.0), 50.0);
    assert_eq!(scaled_amount(100.0, 11.0, 10.0, 0.0), 0.0);
    assert_eq!(scaled_amount(-1.0, 0.0, 10.0, 0.0), 0.0);
    assert_eq!(scaled_amount(f64::NAN, 0.0, 10.0, 0.0), 0.0);
}

#[test]
fn source_guard_rejects_missing_drone_and_stale_npc() {
    let system = crate::space::System::new(1, std::sync::Arc::new(crate::space::Pilots::default()), 0);
    assert!(!SourceGuard::Drone { source: 1, target: 2, generation: 3 }.valid(&system));
    assert!(!SourceGuard::Npc { source: 1, target: 2, generation: 3 }.valid(&system));
}
