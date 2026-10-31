//! Per-system closure mailbox and wall-aligned timer/NPC driver for ballparks
//! whose destiny updates are consumed by `eve/client/script/remote/michelle.py`.
//! The actor owns in-memory `System` state; snapshot jobs persist pilot positions
//! to `game.items`, while NPC effects and deferred jobs run outside the actor.
//! Its `interval_at` starts at the next whole Unix 100 ms boundary and uses
//! `MissedTickBehavior::Skip`. Housekeeping runs every interval; destiny motion
//! advances only on whole-second boundaries. Mailbox messages never evolve balls.
//! The operator-configured mailbox budget controls depth warnings.
//! This module does not decode the client protocol or perform database work
//! inside the actor; timers and non-position dynamic state are not persistent.
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
fn aligned_interval(wall_ms: i64, instant: Instant) -> tokio::time::Interval {
    let offset = next_aligned_ms(wall_ms, TICK_INTERVAL_MS as i64) - wall_ms;
    let mut interval = tokio::time::interval_at(
        instant + Duration::from_millis(offset as u64),
        Duration::from_millis(TICK_INTERVAL_MS),
    );
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    interval
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
        let mut interval = aligned_interval(now_ms(), Instant::now());
        loop {
            tokio::select! {
                message = rx.recv() => match message {
                    Some(job) => job(&mut system),
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
                    if depth as u128 > crate::config::game().space.mailbox_budget as u128 { warn!(system = id, mailbox_depth = depth, "space mailbox exceeds budget"); }
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

#[cfg(test)] mod tests;
