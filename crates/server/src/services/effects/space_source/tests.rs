use super::*;

#[test]
fn missing_sources_never_pass_either_guard() {
    let system = System::new(1, Arc::new(crate::space::Pilots::default()), 0);
    assert!(!SourceGuard::Npc { source: 1, target: 2, generation: 3 }.valid(&system));
    assert!(!SourceGuard::Drone { source: 1, target: 2, generation: 3 }.valid(&system));
}
