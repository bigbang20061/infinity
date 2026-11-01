use super::*;

#[test]
fn aborted_boundary_retires_and_repeat_requires_capture() {
    let stop = AtomicBool::new(false);
    assert!(boundary_continues(true, false, &stop));
    assert!(!boundary_continues(false, false, &stop));
    assert!(!boundary_continues(true, true, &stop));
    stop.store(true, Ordering::Relaxed);
    assert!(!boundary_continues(true, false, &stop));

    let (sender, receiver) = tokio::sync::oneshot::channel::<Option<Vec<crate::services::effects::heat::SourceSnapshot>>>();
    drop(sender);
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let captured = runtime.block_on(async { matches!(receiver.await, Ok(Some(_))) });
    assert!(!boundary_continues(captured, false, &AtomicBool::new(false)));
}
