//! Per-system unbounded closure mailbox and 100 ms simulation driver.
//! Mirrors destiny's ballpark advancement; no database or client protocol decoding.
//! NPC effects are sequential; deferred jobs run concurrently outside the actor.
use super::*;
use std::sync::Weak;
use std::time::Duration;
use tokio::time::{Instant, MissedTickBehavior};
use tracing::{trace, warn};

/// D1 server simulation tick policy.
const TICK_INTERVAL_MS: u64 = 100;
/// Existing server tick-duration budget.
const TICK_BUDGET_MS: u64 = 100;
/// Existing server tick-lateness budget.
const TICK_LATENESS_BUDGET_MS: u64 = 100;
/// D6 server mailbox budget.
const MAILBOX_DEPTH_BUDGET: usize = 10_000;

pub(super) type Job = Box<dyn FnOnce(&mut System) + Send>;
#[derive(Clone)]
pub(super) struct SystemHandle {
    pub(super) tx: mpsc::UnboundedSender<Job>,
    pub(super) gate: Arc<tokio::sync::Mutex<()>>,
    pub(super) written: Arc<std::sync::atomic::AtomicU64>,
}
impl SystemHandle {
    pub(super) fn is_closed(&self) -> bool { self.tx.is_closed() }
    pub(super) fn send(&self, job: Job) -> std::result::Result<(), mpsc::error::SendError<Job>> { self.tx.send(job) }
    pub(super) async fn with<R: Send + 'static>(&self, f: impl FnOnce(&mut System) -> R + Send + 'static) -> Option<R> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.send(Box::new(move |s| { let _ = tx.send(f(s)); })).ok()?;
        rx.await.ok()
    }
}

pub(super) async fn after(handle: Option<SystemHandle>, delay_ms: u64) -> bool {
    let Some(handle) = handle else { return false };
    let (tx, rx) = tokio::sync::oneshot::channel();
    if handle.send(Box::new(move |s| s.schedule(delay_ms, move |_| { let _ = tx.send(()); }))).is_err() {
        return false;
    }
    rx.await.is_ok()
}
pub(super) fn start(mut system: System, id: i64, systems: Weak<Mutex<HashMap<i64, SystemHandle>>>, db: Db) -> SystemHandle {
    npc::start_worker(&mut system);
    let (tx, mut rx) = mpsc::unbounded_channel::<Job>();
    let identity = tx.downgrade();
    let handle = SystemHandle { tx, gate: Arc::new(tokio::sync::Mutex::new(())), written: Arc::new(std::sync::atomic::AtomicU64::new(0)) };
    system.snapshot_context = Some(snapshot::Context {
        tx: handle.tx.downgrade(), gate: handle.gate.clone(), written: handle.written.clone(), db,
    });
    let task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(TICK_INTERVAL_MS));
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                message = rx.recv() => match message {
                    Some(job) => { system.evolve_all(now_ms()); job(&mut system); }
                    None => break,
                },
                scheduled = interval.tick() => {
                    let start = Instant::now();
                    let lateness = start.saturating_duration_since(scheduled);
                    let depth = rx.len();
                    system.tick(now_ms());
                    let duration = start.elapsed();
                    trace!(system = id, duration_us = duration.as_micros() as u64,
                        lateness_us = lateness.as_micros() as u64, mailbox_depth = depth, "space tick");
                    if lateness > Duration::from_millis(TICK_LATENESS_BUDGET_MS) || duration > Duration::from_millis(TICK_BUDGET_MS) {
                        warn!(system = id, ?duration, ?lateness, "space tick exceeds budget");
                    }
                    if depth > MAILBOX_DEPTH_BUDGET { warn!(system = id, mailbox_depth = depth, "space mailbox exceeds budget"); }
                }
            }
        }
    });
    tokio::spawn(async move {
        if let Err(error) = task.await { warn!(system = id, %error, "space actor stopped"); }
        if let Some(systems) = systems.upgrade() {
            let mut systems = systems.lock().unwrap();
            if systems.get(&id).is_some_and(|h| identity.upgrade().is_some_and(|tx| tx.same_channel(&h.tx))) {
                systems.remove(&id);
            }
        }
    });
    handle
}

#[cfg(test)]
mod tests {
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
}
