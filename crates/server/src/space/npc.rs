//! Actor-owned NPC decisions and spawn groups, mirroring entity targeting and
//! destiny Orbit/Stop events. Combat attributes are loaded by services from SDE;
//! Weapon and repair effects use a sequential worker; deferred jobs run concurrently.
//! Neither runs inside the actor.
//! Brains and groups are transient; persistent NPC simulation is not supported.
use super::*;
use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};

/// D9 once-per-second NPC thinking and initial staggering.
const NPC_THINK_INTERVAL_MS: i64 = 1000;
/// Existing server target-loss policy.
const NPC_TARGET_LOSS_RANGE_FACTOR: f64 = 2.0;

static NEXT_GROUP: AtomicU64 = AtomicU64::new(1);

/// Server NPC targeting policy: sentinel owner permitting targets among all pilots.
pub const HOSTILE_TO_ALL: i64 = -1;
pub type NpcFuture = Pin<Box<dyn Future<Output = ()> + Send>>;
/// Called by the worker, not the actor. Capture Db and Weak<World>, upgrading
/// only for a job, so an idle brain cannot keep its own World alive.
pub type NpcEffects = Arc<dyn Fn(NpcAction) -> NpcFuture + Send + Sync>;

pub enum NpcAction {
    Shot { me: Ball, victim: Ball },
    Repair,
}

pub struct NpcConfig {
    pub fly_range: f64,
    pub aggro_range: f64,
    pub rate_of_fire_ms: f64,
    pub scan_resolution: f64,
    /// Shield amount, armor amount, cycle in milliseconds.
    pub repairs: (f64, f64, f64),
}

pub struct NpcBrain {
    pub owner: Option<i64>,
    /// Ship, pilot, lock completion in Unix milliseconds.
    pub target: Option<(i64, i64, i64)>,
    pub announced: bool,
    pub next_shot_ms: i64,
    pub next_repair_ms: i64,
    pub next_think_ms: i64,
    pub config: NpcConfig,
    effects: NpcEffects,
    aggressors: HashSet<i64>,
}
impl NpcBrain {
    pub fn new(owner: Option<i64>, config: NpcConfig, effects: NpcEffects) -> Self {
        Self { owner, config, effects, target: None, announced: false,
            next_shot_ms: 0, next_repair_ms: 0, next_think_ms: 0,
            aggressors: HashSet::new() }
    }
}

pub(super) enum EffectJob {
    Npc { effects: NpcEffects, action: NpcAction },
}
pub(super) fn start_worker(system: &mut System) {
    let (tx, mut rx) = mpsc::unbounded_channel::<EffectJob>();
    let (deferred_tx, mut deferred_rx) = mpsc::unbounded_channel::<NpcFuture>();
    system.effect_jobs = Some(tx);
    system.deferred_jobs = Some(deferred_tx);
    let worker = tokio::spawn(async move {
        while let Some(EffectJob::Npc { effects, action }) = rx.recv().await {
            effects(action).await;
        }
    });
    tokio::spawn(async move {
        if let Err(error) = worker.await { tracing::warn!(%error, "space NPC worker failed"); }
    });
    tokio::spawn(async move {
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                job = deferred_rx.recv() => match job {
                    Some(job) => { tasks.spawn(job); }
                    None => break,
                },
                result = tasks.join_next(), if !tasks.is_empty() => {
                    if let Some(Err(error)) = result { tracing::warn!(%error, "space deferred job failed"); }
                }
            }
        }
        while let Some(result) = tasks.join_next().await {
            if let Err(error) = result { tracing::warn!(%error, "space deferred job failed"); }
        }
    });
}

pub(super) struct SpawnGroup {
    members: HashSet<i64>,
    on_cleared: Box<dyn FnOnce(&mut System) + Send>,
}

impl System {
    /// Attach only to a live ball. First think is staggered over the next second.
    pub fn add_npc(&mut self, id: i64, mut brain: NpcBrain) -> bool {
        if !self.contains(id) { return false; }
        brain.next_think_ms = self.ms + id.rem_euclid(NPC_THINK_INTERVAL_MS);
        self.brains.insert(id, brain);
        true
    }
    pub fn is_npc(&self, id: i64) -> bool { self.brains.contains_key(&id) }
    pub fn add_aggressor(&mut self, id: i64, pilot: i64) {
        if let Some(brain) = self.brains.get_mut(&id) { brain.aggressors.insert(pilot); }
    }
    /// The callback runs once INSIDE this actor (D3): no await, DB or World
    /// calls. It may spawn owned asynchronous work. Missing members count as gone.
    pub fn spawn_group(&mut self, members: impl IntoIterator<Item = i64>, on_cleared: impl FnOnce(&mut System) + Send + 'static) -> u64 {
        let id = NEXT_GROUP.fetch_add(1, Ordering::Relaxed);
        let members: HashSet<_> = members.into_iter().filter(|id| self.contains(*id)).collect();
        if members.is_empty() {
            on_cleared(self);
        } else {
            for member in &members { self.member_groups.entry(*member).or_default().insert(id); }
            self.groups.insert(id, SpawnGroup { members, on_cleared: Box::new(on_cleared) });
        }
        id
    }
    /// Cleared or unknown groups read as zero; handles are process-unique across reloads.
    pub fn group_remaining(&self, id: u64) -> usize {
        self.groups.get(&id).map_or(0, |g| g.members.len())
    }
    pub(super) fn npc_removed(&mut self, id: i64) {
        self.brains.remove(&id);
        if let Some(groups) = self.member_groups.remove(&id) {
            for group in groups {
                let Some(g) = self.groups.get_mut(&group) else { continue };
                g.members.remove(&id);
                if g.members.is_empty() {
                    let g = self.groups.remove(&group).unwrap();
                    (g.on_cleared)(self);
                }
            }
        }
    }
    fn npc_effect(&self, brain: &NpcBrain, action: NpcAction) {
        if let Some(tx) = &self.effect_jobs {
            let _ = tx.send(EffectJob::Npc { effects: brain.effects.clone(), action });
        }
    }
    pub(super) fn think_npcs(&mut self) {
        // Reuse the scratch vector: no per-tick allocation after reaching capacity.
        let mut due = std::mem::take(&mut self.due_brains);
        due.extend(self.brains.iter().filter(|(_, b)| self.ms >= b.next_think_ms).map(|(&id, _)| id));
        for &id in &due {
            let Some(mut brain) = self.brains.remove(&id) else { continue };
            brain.next_think_ms = self.ms + NPC_THINK_INTERVAL_MS;
            self.think_npc(id, &mut brain);
            if self.contains(id) { self.brains.insert(id, brain); }
        }
        due.clear();
        self.due_brains = due;
    }
    fn lose_npc_target(&mut self, id: i64, brain: &mut NpcBrain) {
        if let Some((_, pilot, _)) = brain.target.take() {
            self.notify_pilot(pilot, "OnTarget", vec![Value::str("otherlost"), Value::Int(id), Value::None]);
            self.command(id, |b, stamp, _| {
                b.mode = MODE_STOP;
                b.speed_fraction = 0.0;
                vec![event(stamp, "Stop", vec![Value::Int(b.id)])]
            });
        }
        brain.announced = false;
    }
    fn think_npc(&mut self, id: i64, brain: &mut NpcBrain) {
        let Some(me) = self.ball(id) else { return };
        let pos = me.pos;
        let current = brain.target.and_then(|(ship, _, _)| self.ball(ship))
            .filter(|b| b.cloak == 0 && b.mode != MODE_WARP && b.signature_radius.is_some()
                && len(sub(b.pos, pos)) < brain.config.aggro_range * NPC_TARGET_LOSS_RANGE_FACTOR);
        let victim_id = current.map(|b| b.id);
        let (shield, armor, cycle) = brain.config.repairs;
        if (shield > 0.0 || armor > 0.0) && self.ms >= brain.next_repair_ms {
            brain.next_repair_ms = self.ms + cycle as i64;
            self.npc_effect(brain, NpcAction::Repair);
        }
        if victim_id.is_none() {
            self.lose_npc_target(id, brain);
            let nearest = self.pilots().filter(|b| b.char_id.is_some_and(|pilot|
                brain.owner == Some(HOSTILE_TO_ALL) || brain.owner == Some(pilot) || brain.aggressors.contains(&pilot))
                && b.signature_radius.is_some() && b.cloak == 0 && b.mode != MODE_WARP && len(sub(b.pos, pos)) <= brain.config.aggro_range)
                .min_by(|a, b| len(sub(a.pos, pos)).total_cmp(&len(sub(b.pos, pos))))
                .map(|b| (b.id, b.char_id.unwrap(), b.signature_radius.unwrap()));
            if let Some((ship, pilot, signature)) = nearest {
                let orbit = brain.config.fly_range;
                self.command(id, move |b, stamp, _| {
                    b.mode = MODE_ORBIT;
                    b.follow_id = ship;
                    b.follow_range = orbit as f32;
                    b.speed_fraction = 1.0;
                    vec![event(stamp, "Orbit", vec![Value::Int(id), Value::Int(ship), Value::Float(orbit)]),
                        event(stamp, "SetSpeedFraction", vec![Value::Int(id), Value::Float(1.0)])]
                });
                let locked = self.ms.saturating_add(lock_time_ms(brain.config.scan_resolution, signature) as i64);
                brain.target = Some((ship, pilot, locked));
                brain.announced = false;
                brain.next_shot_ms = locked;
                self.schedule_at(locked, move |s| s.complete_npc_lock(id, ship, pilot, locked));
            }
            return;
        }
        let (ship, _, _) = brain.target.unwrap();
        if brain.announced && self.ms >= brain.next_shot_ms {
            self.npc_effect(brain, NpcAction::Shot { me: self.ball(id).unwrap().clone(), victim: self.ball(ship).unwrap().clone() });
            brain.next_shot_ms = self.ms + brain.config.rate_of_fire_ms as i64;
        }
    }
    fn complete_npc_lock(&mut self, id: i64, ship: i64, pilot: i64, deadline: i64) {
        let Some(mut brain) = self.brains.remove(&id) else { return };
        let current = brain.target == Some((ship, pilot, deadline)) && !brain.announced;
        let valid = current && self.ball(id).zip(self.ball(ship)).is_some_and(|(me, victim)|
                victim.char_id == Some(pilot) && victim.signature_radius.is_some()
                && victim.cloak == 0 && victim.mode != MODE_WARP
                && len(sub(victim.pos, me.pos)) < brain.config.aggro_range * NPC_TARGET_LOSS_RANGE_FACTOR);
        if valid {
            self.notify_pilot(pilot, "OnTarget", vec![Value::str("otheradd"), Value::Int(id)]);
            brain.announced = true;
            self.npc_effect(&brain, NpcAction::Shot { me: self.ball(id).unwrap().clone(), victim: self.ball(ship).unwrap().clone() });
            brain.next_shot_ms = self.ms + brain.config.rate_of_fire_ms as i64;
        } else if current {
            self.lose_npc_target(id, &mut brain);
        }
        self.brains.insert(id, brain);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn ball(id: i64, pilot: Option<i64>, x: f64) -> Ball {
        Ball {
            id, type_id: 1, owner_id: 1, corp_id: 1, alliance_id: -1,
            char_id: pilot, signature_radius: pilot.map(|_| 1.0), mode: MODE_STOP, radius: 1.0, mass: 1e6,
            max_vel: 0.0, agility: 1.0, speed_fraction: 0.0,
            pos: [x, 0.0, 0.0], vel: [0.0; 3], goto: [0.0; 3],
            follow_id: 0, follow_range: 0.0, free: true, global: false,
            massive: true, warp: None, warp_factor: 0, last_ms: 0,
            cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0,
            slim: Vec::new(),
        }
    }
    fn setup(owner: Option<i64>) -> (System, mpsc::UnboundedReceiver<EffectJob>, mpsc::UnboundedReceiver<(String, Vec<Value>)>) {
        let registry = Arc::new(Pilots::default());
        let (notify, notifications) = mpsc::unbounded_channel();
        registry.register_pilot(10, notify);
        let mut s = System::new(1, registry);
        let (tx, rx) = mpsc::unbounded_channel();
        s.effect_jobs = Some(tx);
        s.insert(ball(1000, None, 0.0));
        s.add_npc(1000, NpcBrain::new(owner, NpcConfig {
            fly_range: 20.0, aggro_range: 100.0, rate_of_fire_ms: 2500.0, scan_resolution: 17_163.0,
            repairs: (0.0, 0.0, 5000.0),
        }, Arc::new(|_| panic!("effects must run outside the actor"))));
        (s, rx, notifications)
    }
    #[test]
    fn nearest_hostile_excludes_cloak_warp_and_outside_aggro() {
        let (mut s, _, _) = setup(Some(10));
        s.insert(ball(1, Some(11), 1.0)); // neutral
        let mut cloaked = ball(2, Some(10), 2.0);
        cloaked.cloak = 1;
        s.insert(cloaked);
        let mut warping = ball(3, Some(12), 3.0);
        warping.mode = MODE_WARP;
        s.insert(warping);
        s.insert(ball(4, Some(13), 101.0));
        s.add_aggressor(1000, 12);
        s.add_aggressor(1000, 13);
        s.tick(0);
        assert!(s.brains[&1000].target.is_none());
        s.insert(ball(5, Some(14), 40.0));
        s.insert(ball(6, Some(15), 60.0));
        s.add_aggressor(1000, 14);
        s.add_aggressor(1000, 15);
        s.tick(1000);
        assert_eq!(s.brains[&1000].target.unwrap().0, 5);
        assert_eq!(s.ball(1000).unwrap().mode, MODE_ORBIT);
        assert_eq!(s.ball(1000).unwrap().speed_fraction, 1.0);
    }
    #[test]
    fn owner_and_hostile_to_all_and_aggro_boundary() {
        for owner in [Some(10), Some(HOSTILE_TO_ALL)] {
            let (mut s, _, _) = setup(owner);
            s.insert(ball(1, Some(10), 100.0));
            s.tick(0);
            assert_eq!(s.brains[&1000].target.unwrap().0, 1);
        }
    }
    #[test]
    fn target_loss_stops_and_notifies_beyond_double_aggro() {
        let (mut s, _, mut notify) = setup(Some(HOSTILE_TO_ALL));
        s.insert(ball(1, Some(10), 50.0));
        let (tx, mut events) = mpsc::unbounded_channel();
        s.observe(1, 1, tx);
        s.tick(0);
        events.try_recv().unwrap();
        s.command(1, |b, _, _| { b.pos = [201.0, 0.0, 0.0]; vec![] });
        s.tick(1000);
        assert!(s.brains[&1000].target.is_none());
        assert_eq!(s.ball(1000).unwrap().mode, MODE_STOP);
        assert_eq!(s.ball(1000).unwrap().speed_fraction, 0.0);
        let batch = events.try_recv().unwrap();
        assert!(batch.iter().any(|v| {
            let Value::Tuple(parts) = v else { return false };
            let Value::Tuple(call) = &parts[1] else { return false };
            call[0].as_bytes() == Some(b"Stop")
        }));
        let (name, args) = notify.try_recv().unwrap();
        assert_eq!(name, "OnTarget");
        assert_eq!(args[0].as_bytes(), Some(b"otherlost".as_slice()));
    }
    #[test]
    fn lock_delay_and_shot_throttling() {
        let (mut s, mut jobs, _) = setup(Some(HOSTILE_TO_ALL));
        s.insert(ball(1, Some(10), 50.0));
        s.tick(0);
        let deadline = lock_time_ms(17_163.0, 1.0) as i64;
        s.tick(deadline - 1);
        assert!(jobs.try_recv().is_err());
        s.tick(deadline);
        assert!(matches!(jobs.try_recv().unwrap(), EffectJob::Npc { action: NpcAction::Shot { .. }, .. }));
        assert!(s.brains[&1000].announced);
        s.tick(deadline + 2000);
        assert!(jobs.try_recv().is_err());
        s.tick(deadline + 3000);
        assert!(matches!(jobs.try_recv().unwrap(), EffectJob::Npc { action: NpcAction::Shot { .. }, .. }));
        assert!(jobs.try_recv().is_err());
    }

    #[test]
    fn stale_and_removed_targets_do_not_complete_locks() {
        for remove_npc in [false, true] {
            let (mut s, mut jobs, mut notifications) = setup(Some(HOSTILE_TO_ALL));
            s.insert(ball(1, Some(10), 50.0));
            s.tick(0);
            let deadline = s.brains[&1000].target.unwrap().2;
            if remove_npc { s.remove_ball(1000); } else { s.remove_ball(1); }
            s.tick(deadline);
            assert!(jobs.try_recv().is_err());
            while let Ok((_, args)) = notifications.try_recv() {
                assert_ne!(args[0].as_bytes(), Some(b"otheradd".as_slice()));
            }
        }
        let (mut s, mut jobs, _) = setup(Some(HOSTILE_TO_ALL));
        s.insert(ball(1, Some(10), 50.0));
        s.tick(0);
        let deadline = s.brains[&1000].target.unwrap().2;
        s.brains.get_mut(&1000).unwrap().target = Some((1, 10, deadline + 100));
        s.tick(deadline);
        assert!(jobs.try_recv().is_err());
    }

    #[test]
    fn failed_lock_reacquires_and_ignores_stale_completion() {
        let (mut s, mut jobs, mut notifications) = setup(Some(HOSTILE_TO_ALL));
        s.insert(ball(1, Some(10), 50.0));
        s.tick(0);
        let old_deadline = s.brains[&1000].target.unwrap().2;
        s.brains.get_mut(&1000).unwrap().next_think_ms = old_deadline + 1;
        s.command(1, |b, _, _| { b.cloak = 1; vec![] });
        s.tick(old_deadline);
        assert!(s.brains[&1000].target.is_none());
        assert!(!s.brains[&1000].announced);
        assert_eq!(s.ball(1000).unwrap().mode, MODE_STOP);
        assert_eq!(notifications.try_recv().unwrap().1[0].as_bytes(), Some(b"otherlost".as_slice()));
        assert!(jobs.try_recv().is_err());

        s.command(1, |b, _, _| { b.cloak = 0; vec![] });
        s.tick(old_deadline + 1);
        let new_target = s.brains[&1000].target.unwrap();
        assert!(new_target.2 > old_deadline);
        s.complete_npc_lock(1000, 1, 10, old_deadline);
        assert_eq!(s.brains[&1000].target, Some(new_target));
        assert!(!s.brains[&1000].announced);
        assert!(jobs.try_recv().is_err());
        s.tick(new_target.2);
        assert!(s.brains[&1000].announced);
        assert_eq!(notifications.try_recv().unwrap().1[0].as_bytes(), Some(b"otheradd".as_slice()));
        assert!(matches!(jobs.try_recv().unwrap(), EffectJob::Npc { action: NpcAction::Shot { .. }, .. }));
    }

    #[tokio::test]
    async fn deferred_jobs_progress_while_npc_effects_remain_fifo() {
        let mut s = System::default();
        start_worker(&mut s);
        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let release = Arc::new(Mutex::new(Some(release_rx)));
        let effects: NpcEffects = Arc::new(move |_| {
            let started_tx = started_tx.clone();
            let release = release.lock().unwrap().take();
            Box::pin(async move {
                started_tx.send(()).unwrap();
                if let Some(release) = release { let _ = release.await; }
            })
        });
        let tx = s.effect_jobs.as_ref().unwrap();
        assert!(tx.send(EffectJob::Npc { effects: effects.clone(), action: NpcAction::Repair }).is_ok());
        assert!(tx.send(EffectJob::Npc { effects, action: NpcAction::Repair }).is_ok());
        started_rx.recv().await.unwrap();
        let (blocked_tx, blocked_rx) = tokio::sync::oneshot::channel();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        s.defer(async move { let _ = entered_tx.send(()); let _ = blocked_rx.await; });
        entered_rx.await.unwrap();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        s.defer(async move { let _ = done_tx.send(()); });
        done_rx.await.unwrap();
        blocked_tx.send(()).unwrap();
        assert!(started_rx.try_recv().is_err());
        release_tx.send(()).unwrap();
        started_rx.recv().await.unwrap();
    }

    #[tokio::test]
    async fn timer_defers_async_work_without_polling_inside_actor() {
        let (mut s, _jobs, _) = setup(None);
        let (tx, mut deferred_jobs) = mpsc::unbounded_channel();
        s.deferred_jobs = Some(tx);
        let count = Arc::new(AtomicUsize::new(0));
        let deferred = count.clone();
        s.schedule(100, move |s| {
            s.defer(async move { deferred.fetch_add(1, Ordering::Relaxed); });
        });
        s.tick(100);
        assert_eq!(count.load(Ordering::Relaxed), 0);
        let future = deferred_jobs.try_recv().unwrap();
        future.await;
        assert_eq!(count.load(Ordering::Relaxed), 1);
        assert!(deferred_jobs.try_recv().is_err());
    }
    #[test]
    fn groups_clear_once_for_destroyed_removed_and_missing_members() {
        let (mut s, _, _) = setup(None);
        assert!(!s.is_cold());
        s.insert(ball(2, None, 0.0));
        let cleared = Arc::new(AtomicUsize::new(0));
        let count = cleared.clone();
        let group = s.spawn_group([1000, 2, 999], move |s| {
            assert!(!s.contains(1000) && !s.contains(2));
            count.fetch_add(1, Ordering::Relaxed);
        });
        assert_eq!(s.group_remaining(group), 2);
        s.destroy_ball(1000);
        assert!(!s.is_npc(1000));
        assert_eq!(s.group_remaining(group), 1);
        assert_eq!(cleared.load(Ordering::Relaxed), 0);
        s.remove_ball(2);
        s.remove_ball(2);
        s.destroy_ball(1000);
        assert_eq!(s.group_remaining(group), 0);
        assert_eq!(cleared.load(Ordering::Relaxed), 1);
        let count = cleared.clone();
        let empty = s.spawn_group([], move |_| { count.fetch_add(1, Ordering::Relaxed); });
        assert_eq!(s.group_remaining(empty), 0);
        assert_eq!(cleared.load(Ordering::Relaxed), 2);
    }
    #[test]
    fn group_handles_are_unique_across_system_reloads() {
        let (mut before, _, _) = setup(None);
        let old = before.spawn_group([1000], |_| {});
        let (mut after, _, _) = setup(None);
        let new = after.spawn_group([1000], |_| {});
        assert_ne!(old, new);
        assert_eq!(after.group_remaining(old), 0);
        assert_eq!(after.group_remaining(new), 1);
    }
}
