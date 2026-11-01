use super::*;

fn marker(at: i64, module: i64, generation: &Arc<AtomicBool>, cycle_start: i64, kind: BoundaryKind) -> Boundary {
    Boundary { at, module, generation: generation.clone(), cycle_start, kind, ready: false }
}

#[test]
fn ordered_and_idempotent_install_preserves_readiness() {
    let generation = Arc::new(AtomicBool::new(true));
    let mut queue = BoundaryQueue::default();
    queue.install(marker(20, 1, &generation, 20, BoundaryKind::Start));
    queue.install(marker(10, 2, &generation, 10, BoundaryKind::Start));
    queue.install(marker(10, 1, &generation, 10, BoundaryKind::Start));
    assert!(queue.mark_ready(1, &generation, 10, BoundaryKind::Start));
    queue.install(marker(10, 1, &generation, 10, BoundaryKind::Start));
    let first = queue.pop_ready(10).unwrap();
    assert_eq!((first.at, first.module), (10, 1));
    assert!(queue.pop_ready(i64::MAX).is_none());
    assert!(queue.mark_ready(2, &generation, 10, BoundaryKind::Start));
    assert_eq!(queue.pop_ready(10).unwrap().module, 2);
    assert!(queue.mark_ready(1, &generation, 20, BoundaryKind::Start));
    assert_eq!(queue.pop_ready(20).unwrap().at, 20);
    assert!(queue.pop_ready(19).is_none());
    assert!(queue.pop_ready(20).is_none());
}

#[test]
fn later_ready_waits_and_limit_tracks_oldest() {
    let generation = Arc::new(AtomicBool::new(true));
    let mut queue = BoundaryQueue::default();
    queue.install(marker(10, 1, &generation, 10, BoundaryKind::Start));
    queue.install(marker(20, 2, &generation, 20, BoundaryKind::Start));
    assert!(queue.mark_ready(2, &generation, 20, BoundaryKind::Start));
    assert_eq!(queue.limit(5), 5);
    assert_eq!(queue.limit(30), 10);
    assert!(queue.pop_ready(i64::MAX).is_none());
    assert_eq!(queue.remove_cycle(1, &generation, 10), 1);
    assert_eq!(queue.limit(30), 20);
    assert!(queue.pop_ready(19).is_none());
    assert_eq!(queue.pop_ready(20).unwrap().module, 2);
    assert_eq!(queue.limit(30), 30);
}

#[test]
fn end_precedes_next_start_at_same_timestamp() {
    let generation = Arc::new(AtomicBool::new(true));
    let mut queue = BoundaryQueue::default();
    queue.install(marker(20, 1, &generation, 20, BoundaryKind::Start));
    queue.install(marker(20, 1, &generation, 10, BoundaryKind::End));
    assert!(queue.mark_ready(1, &generation, 20, BoundaryKind::Start));
    assert!(queue.pop_ready(20).is_none());
    assert!(queue.mark_ready(1, &generation, 10, BoundaryKind::End));
    assert!(queue.pop_ready(19).is_none());
    assert_eq!(queue.pop_ready(20).unwrap().kind, BoundaryKind::End);
    assert_eq!(queue.pop_ready(20).unwrap().kind, BoundaryKind::Start);
}

#[test]
fn generations_are_pointer_identified_and_cleaned_precisely() {
    let old = Arc::new(AtomicBool::new(false));
    let replacement = Arc::new(AtomicBool::new(false));
    let stale = Arc::new(AtomicBool::new(false));
    let mut queue = BoundaryQueue::default();
    queue.install(marker(10, 1, &old, 10, BoundaryKind::Start));
    queue.install(marker(10, 1, &replacement, 10, BoundaryKind::Start));
    queue.install(marker(20, 1, &old, 10, BoundaryKind::End));
    queue.install(marker(30, 2, &old, 30, BoundaryKind::Start));
    assert!(!queue.mark_ready(1, &stale, 10, BoundaryKind::Start));
    assert_eq!(queue.remove_generation(1, &old), 2);
    assert!(queue.mark_ready(1, &replacement, 10, BoundaryKind::Start));
    assert!(Arc::ptr_eq(&queue.pop_ready(10).unwrap().generation, &replacement));
    assert_eq!(queue.remove_module(2), 1);
    assert_eq!(queue.remove_module(2), 0);
    assert_eq!(queue.limit(40), 40);
}

#[test]
fn future_ready_marker_is_not_due() {
    let generation = Arc::new(AtomicBool::new(true));
    let mut queue = BoundaryQueue::default();
    queue.install(marker(100, 1, &generation, 10, BoundaryKind::End));
    assert!(queue.mark_ready(1, &generation, 10, BoundaryKind::End));
    assert!(queue.pop_ready(0).is_none());
    assert!(queue.pop_ready(99).is_none());
    assert_eq!(queue.limit(200), 100);
    assert!(queue.has_cycle(1, &generation, 10));
    assert!(!queue.has_cycle(1, &generation, 11));
    assert_eq!(queue.pop_ready(100).unwrap().at, 100);
    assert!(!queue.has_cycle(1, &generation, 10));
}
