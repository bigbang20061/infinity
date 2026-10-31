//! Actor-owned NPC decisions and spawn groups, mirroring entity targeting and
//! destiny Orbit/Stop events. Combat attributes are loaded by services from SDE;
//! Weapon and repair effects use a sequential worker; deferred jobs run concurrently.
//! Neither runs inside the actor.
//! Targeting uses positions sampled at the actor's clock; shot jobs capture those
//! positions before the effects worker runs.
//! Think intervals and target-loss range are controlled by the combat configuration.
//! Brains and groups are transient; persistent NPC simulation is not supported.
use super::*;
use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};

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
    /// Attach only to a live ball. First think is staggered over the configured interval.
    pub fn add_npc(&mut self, id: i64, mut brain: NpcBrain) -> bool {
        if !self.contains(id) { return false; }
        brain.next_think_ms = self.ms.saturating_add(id.rem_euclid(crate::config::game().combat.npc_think_ms));
        self.brains.insert(id, brain);
        true
    }
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
            brain.next_think_ms = self.ms.saturating_add(crate::config::game().combat.npc_think_ms);
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
        let pos = me.position_at(self.ms);
        let current = brain.target.and_then(|(ship, _, _)| self.ball(ship))
            .filter(|b| b.cloak == 0 && b.mode != MODE_WARP && b.signature_radius.is_some()
                && len(sub(b.position_at(self.ms), pos)) < brain.config.aggro_range * crate::config::game().combat.npc_target_loss_range_factor);
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
                && b.signature_radius.is_some() && b.cloak == 0 && b.mode != MODE_WARP)
                .map(|b| (b, len(sub(b.position_at(self.ms), pos))))
                .filter(|(_, distance)| *distance <= brain.config.aggro_range)
                .min_by(|(_, a), (_, b)| a.total_cmp(b))
                .map(|(b, _)| (b.id, b.char_id.unwrap(), b.signature_radius.unwrap()));
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
            self.npc_effect(brain, NpcAction::Shot { me: self.sampled_ball(id).unwrap(), victim: self.sampled_ball(ship).unwrap() });
            brain.next_shot_ms = self.ms + brain.config.rate_of_fire_ms as i64;
        }
    }
    fn complete_npc_lock(&mut self, id: i64, ship: i64, pilot: i64, deadline: i64) {
        let Some(mut brain) = self.brains.remove(&id) else { return };
        let current = brain.target == Some((ship, pilot, deadline)) && !brain.announced;
        let valid = current && self.ball(id).zip(self.ball(ship)).is_some_and(|(me, victim)|
                victim.char_id == Some(pilot) && victim.signature_radius.is_some()
                && victim.cloak == 0 && victim.mode != MODE_WARP
                && len(sub(victim.position_at(self.ms), me.position_at(self.ms))) < brain.config.aggro_range * crate::config::game().combat.npc_target_loss_range_factor);
        if valid {
            self.notify_pilot(pilot, "OnTarget", vec![Value::str("otheradd"), Value::Int(id)]);
            brain.announced = true;
            self.npc_effect(&brain, NpcAction::Shot { me: self.sampled_ball(id).unwrap(), victim: self.sampled_ball(ship).unwrap() });
            brain.next_shot_ms = self.ms + brain.config.rate_of_fire_ms as i64;
        } else if current {
            self.lose_npc_target(id, &mut brain);
        }
        self.brains.insert(id, brain);
    }
}

#[cfg(test)] mod tests;
