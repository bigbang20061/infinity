//! Per-system unbounded closure mailbox and 100 ms simulation driver.
//! Mirrors destiny's ballpark advancement; no database or client protocol decoding.
//! NPC effects use one sequential worker per system; closure timers are not implemented.
use super::*;
use std::sync::Weak;
use std::time::Duration;
use tokio::time::{Instant, MissedTickBehavior};
use tracing::{trace, warn};

type Job = Box<dyn FnOnce(&mut System) + Send>;
#[derive(Clone)]
pub(super) struct SystemHandle(mpsc::UnboundedSender<Job>);
impl SystemHandle {
    pub(super) fn is_closed(&self) -> bool { self.0.is_closed() }
    pub(super) fn send(&self, job: Job) -> std::result::Result<(), mpsc::error::SendError<Job>> { self.0.send(job) }
}
pub(super) fn start(mut system: System, id: i64, systems: Weak<Mutex<HashMap<i64, SystemHandle>>>) -> SystemHandle {
    npc::start_worker(&mut system);
    let (tx, mut rx) = mpsc::unbounded_channel::<Job>();
    let identity = tx.downgrade();
    let task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(100));
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
                    if lateness > Duration::from_millis(100) || duration > Duration::from_millis(100) {
                        warn!(system = id, ?duration, ?lateness, "space tick exceeds budget");
                    }
                    if depth > 10_000 { warn!(system = id, mailbox_depth = depth, "space mailbox exceeds budget"); }
                }
            }
        }
    });
    tokio::spawn(async move {
        if let Err(error) = task.await { warn!(system = id, %error, "space actor stopped"); }
        if let Some(systems) = systems.upgrade() {
            let mut systems = systems.lock().unwrap();
            if systems.get(&id).is_some_and(|h| identity.upgrade().is_some_and(|tx| tx.same_channel(&h.0))) {
                systems.remove(&id);
            }
        }
    });
    SystemHandle(tx)
}
