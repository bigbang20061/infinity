use super::*;

#[tokio::test]
async fn waits_follow_timer_execution_and_actor_lifetime() {
    assert!(!after(None, 100).await);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let handle = SystemHandle { tx, gate: Arc::new(tokio::sync::Mutex::new(())), written: Arc::new(std::sync::atomic::AtomicU64::new(0)) };
    let waiter = tokio::spawn(after(Some(handle.clone()), 100));
    let mut system = System::default();
    rx.recv().await.unwrap()(&mut system);
    system.tick(99);
    assert!(!waiter.is_finished());
    system.tick(100);
    assert!(waiter.await.unwrap());

    let waiter = tokio::spawn(after(Some(handle.clone()), 100));
    rx.recv().await.unwrap()(&mut system);
    drop(system);
    assert!(!waiter.await.unwrap());
    drop(rx);
    assert!(!after(Some(handle), 100).await);
}
