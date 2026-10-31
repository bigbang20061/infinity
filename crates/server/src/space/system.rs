//! Actor-owned ballpark membership, visibility, actions, motion and timers,
//! serving destiny updates consumed by `eve/client/script/remote/michelle.py`.
//! Celestials are supplied from the SDE; dynamic state and timers are in memory.
//! Changed pilot positions are saved to `game.items` by the snapshot worker.
//!
//! One FIFO holds captured AddBalls updates and mode, propulsion, cloak and
//! missile-launch actions. Membership changes immediately, while AddBalls
//! captures its wire state at enqueue. An action enqueued at logical time C is
//! stamped C and applied at the next boundary before evolution to C+1.
//! `current_time` starts at load-time Unix seconds and advances once per fixed
//! one-second evolution, independently of the wall clock. Timers and NPC
//! decisions run on 100 ms housekeeping ticks; snapshots follow motion.
//! After more than five due steps, catchup clamps to one logical step and the
//! next deadline is the next whole wall second, an intentional departure from
//! native cadence. The clamped interpolation anchor is actual execution time;
//! the logical counter is not reset to wall time.
//!
//! DB effects run on workers; timers, warp-exit callbacks and non-position
//! dynamic state do not survive restart. Legacy sphere collisions do not emit bumps.
use super::*;
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::future::Future;

/// carbonengine destiny Ballpark.cpp tick catchup policy.
const MAX_CATCHUP_STEPS: i64 = 5;
/// Destiny destruction effect EXPLOSION.
const DESTRUCTION_EFFECT_EXPLOSION: i64 = 3;
/// Server policy for targeted missiles: destiny/src/Thunkers.cpp LaunchMissile
/// uses aimed launches to initialize direction toward an uncloaked target.
const MISSILE_AIMED_LAUNCH: bool = true;
/// Server policy for ordinary missiles; destiny/src/Thunkers.cpp sets ball massiveness from this flag.
const MISSILE_MASSIVE: bool = false;
pub(crate) type Timer = Box<dyn FnOnce(&mut System) + Send>;
type Action = Box<dyn FnOnce(&mut System, i64) + Send>;

#[derive(Default)]
pub struct System {
    pub(super) id: i64,
    balls: HashMap<i64, Ball>,
    pilots: HashMap<i64, i64>,
    observers: HashMap<i64, (i64, DestinyTx)>,
    registry: Arc<Pilots>,
    target_states: HashMap<i64, TargetState>,
    free_ids: Vec<i64>,
    collision_scratch: Vec<collision::Snapshot>,
    exited_ids: Vec<i64>,
    /// Celestial warp-in points (warpin.rs), by celestial ID.
    warp_ins: HashMap<i64, V3>,
    pending: Option<HashMap<i64, Vec<Value>>>,
    timers: BTreeMap<(i64, u64), Timer>,
    warp_exits: HashMap<i64, Timer>,
    timer_sequence: u64,
    pub(super) ms: i64,
    current_time: i64,
    next_tick_ms: Option<i64>,
    actions: VecDeque<(i64, Action)>,
    pending_uncloak: HashSet<i64>,
    pub(super) brains: HashMap<i64, NpcBrain>,
    pub(super) effect_jobs: Option<mpsc::UnboundedSender<npc::EffectJob>>,
    pub(super) deferred_jobs: Option<mpsc::UnboundedSender<npc::NpcFuture>>,
    pub(super) groups: HashMap<u64, npc::SpawnGroup>,
    pub(super) member_groups: HashMap<i64, std::collections::HashSet<u64>>,
    pub(super) due_brains: Vec<i64>,
    pub(super) snapshot_context: Option<snapshot::Context>,
    pub(super) snapshot_positions: HashMap<i64, (u64, V3)>,
    pub(super) snapshot_generation: u64,
    pub(super) snapshot_due: i64,
}
impl System {
    pub fn new(id: i64, registry: Arc<Pilots>, unix_ms: i64) -> Self {
        let current_time = unix_ms.div_euclid(MILLISECONDS_PER_SECOND);
        Self { id, registry, ms: unix_ms, current_time,
            next_tick_ms: Some(next_aligned_ms(unix_ms, MILLISECONDS_PER_SECOND)), ..Self::default() }
    }
    pub fn current_time(&self) -> i64 { self.current_time }
    /// Schedule relative to this actor's current millisecond clock. Due timers
    /// run only on ticks, ordered by deadline and then insertion order (also for
    /// zero delays). D3 applies: no await, DB access or calls back into World.
    pub fn schedule(&mut self, delay_ms: u64, f: impl FnOnce(&mut System) + Send + 'static) {
        self.schedule_at(self.ms.saturating_add(i64::try_from(delay_ms).unwrap_or(i64::MAX)), f);
    }
    /// Queue an absolute Unix-millisecond deadline, ordered by insertion on ties.
    pub fn schedule_at(&mut self, due: i64, f: impl FnOnce(&mut System) + Send + 'static) {
        let sequence = self.timer_sequence;
        self.timer_sequence = sequence.checked_add(1).expect("space timer sequence exhausted");
        self.timers.insert((due, sequence), Box::new(f));
    }
    /// Actor simulation time in Unix milliseconds.
    pub fn now_ms(&self) -> i64 { self.ms }
    /// Submit owned async work to the concurrent worker outside the actor.
    pub fn defer(&self, job: impl Future<Output = ()> + Send + 'static) {
        if self.deferred_jobs.as_ref().is_none_or(|tx| tx.send(Box::pin(job)).is_err()) {
            tracing::warn!(system = self.id, "space effects worker unavailable");
        }
    }
    pub fn warp_in(&self, id: i64) -> Option<V3> { self.warp_ins.get(&id).copied() }
    /// Queue cancellation of any previous landing intent before a new warp request.
    pub fn clear_warp_exit(&mut self, id: i64) {
        self.actions.push_back((self.current_time, Box::new(move |s, _| { s.warp_exits.remove(&id); })));
    }

    /// Register the latest landing intent after its warp command in the FIFO.
    /// The registration must follow a valid warp request in the same FIFO.
    /// The production service validates before uncloak, clearing, and registration.
    /// A valid duplicate native request may replace the latest consumer's intent.
    pub fn on_warp_exit(&mut self, id: i64, f: impl FnOnce(&mut System) + Send + 'static) {
        self.actions.push_back((self.current_time, Box::new(move |s, _| {
            match s.balls.get(&id).map(|b| b.mode) {
                Some(MODE_WARP) => { s.warp_exits.insert(id, Box::new(f)); }
                Some(MODE_GOTO) => f(s),
                _ => {}
            }
        })));
    }
    pub(super) fn set_warp_in(&mut self, id: i64, point: V3) { self.warp_ins.insert(id, point); }
    pub fn ball(&self, id: i64) -> Option<&Ball> {
        self.balls.get(&id)
    }

    /// Sample the completed segment at the actor's shared millisecond clock.
    pub fn sample_position(&self, id: i64) -> Option<V3> {
        self.ball(id).map(|ball| ball.position_at(self.ms))
    }

    /// Clone only the requested ball and materialize its sampled position.
    pub fn sampled_ball(&self, id: i64) -> Option<Ball> {
        self.ball(id).map(|ball| {
            let mut sampled = ball.clone();
            sampled.pos = ball.position_at(self.ms);
            sampled
        })
    }

    pub fn balls(&self) -> impl Iterator<Item = &Ball> {
        self.balls.values()
    }

    pub fn pilots(&self) -> impl Iterator<Item = &Ball> {
        self.pilots.values().filter_map(|id| self.ball(*id))
    }

    pub fn pilot_ball(&self, char_id: i64) -> Option<&Ball> {
        self.pilots.get(&char_id).and_then(|id| self.ball(*id))
    }

    pub fn contains(&self, id: i64) -> bool {
        self.balls.contains_key(&id)
    }

    pub fn insert(&mut self, mut ball: Ball) {
        ball.last_ms = self.current_time * MILLISECONDS_PER_SECOND;
        ball.interpolation_anchor_ms = self.ms;
        ball.old_pos = ball.pos;
        ball.old_vel = ball.vel;
        ball.segment_acceleration = None;
        self.take(ball.id);
        if let Some(char_id) = ball.char_id {
            self.pilots.insert(char_id, ball.id);
            self.snapshot_generation = self.snapshot_generation.checked_add(1).expect("snapshot generation exhausted");
            self.snapshot_positions.insert(ball.id, (self.snapshot_generation, ball.pos));
            if self.snapshot_due == 0 {
                self.snapshot_due = self.ms.saturating_add(crate::config::game().space.position_snapshot_secs.saturating_mul(MILLISECONDS_PER_SECOND));
            }
        }
        self.balls.insert(ball.id, ball);
    }

    pub fn take(&mut self, id: i64) -> Option<Ball> {
        self.warp_exits.remove(&id);
        let ball = self.balls.remove(&id)?;
        self.stop_all_followers(id);
        self.snapshot_positions.remove(&id);
        if let Some(char_id) = ball.char_id {
            if self.pilots.get(&char_id) == Some(&id) {
                self.pilots.remove(&char_id);
            }
        }
        self.npc_removed(id);
        Some(ball)
    }

    /// Ballpark::StopAllFollowers, for followers of a removed target.
    fn stop_all_followers(&mut self, id: i64) {
        for ball in self.balls.values_mut().filter(|b| b.follow_id == id && matches!(b.mode, MODE_FOLLOW | MODE_ORBIT | MODE_MISSILE)) {
            if ball.mode == MODE_MISSILE {
                ball.massive = false;
                ball.goto_direction(ball.vel);
            } else if ball.mode == MODE_ORBIT && ball.interactive { ball.goto_direction(ball.vel); } else { ball.stop(); }
        }
    }

    pub fn notify_pilot(&self, char_id: i64, name: &str, args: Vec<Value>) {
        self.registry.notify_pilot(char_id, name, args);
    }
    fn population(&self) -> usize {
        self.balls.values().filter(|b| b.char_id.is_some()).count()
    }
    pub fn request_entry(&self, char_id: i64) -> Result<(), i64> {
        self.registry.request_entry(self.id, char_id, self.population())
    }
    pub(super) fn is_cold(&self) -> bool {
        self.actions.is_empty()
            && !self.timers.first_key_value().is_some_and(|(&(due, _), _)| due <= self.ms)
            && self.brains.is_empty() && self.observers.is_empty() && !self.balls.values().any(|b|
            (b.free && (b.mode != MODE_STOP || b.vel != [0.0; 3]))
            || (b.cloak != 0 && b.cloak_until > 0))
            && !collision::stationary_contact(&self.balls)
    }
    pub fn tick(&mut self, ms: i64) {
        self.observers.retain(|_, (_, tx)| !tx.is_closed());
        self.ms = ms;
        let next_tick_ms = *self.next_tick_ms.get_or_insert(MILLISECONDS_PER_SECOND);
        let due = if ms >= next_tick_ms {
            (ms - next_tick_ms) / MILLISECONDS_PER_SECOND + 1
        } else { 0 };
        let steps = if due > MAX_CATCHUP_STEPS {
            tracing::warn!(system = self.id, due, "destiny tick catchup clamped");
            self.next_tick_ms = Some(next_aligned_ms(ms, MILLISECONDS_PER_SECOND));
            1
        } else {
            self.next_tick_ms = Some(next_tick_ms + due * MILLISECONDS_PER_SECOND);
            due
        };
        if !self.is_cold() { self.pending = Some(HashMap::new()); }
        for step in 0..steps {
            let count = self.actions.len();
            for _ in 0..count {
                let (stamp, action) = self.actions.pop_front().unwrap();
                action(self, stamp);
            }
            if self.balls.values().any(|b| b.free && (b.mode != MODE_STOP || b.vel != [0.0; 3]))
                || collision::stationary_contact(&self.balls) {
                self.target_states.clear();
                self.target_states.extend(self.balls.iter().map(|(&id, b)| (id, TargetState {
                    pos: b.pos, vel: b.vel, radius: b.radius,
                })));
                self.free_ids.clear();
                self.free_ids.extend(self.balls.iter().filter_map(|(&id, b)| b.free.then_some(id)));
                self.free_ids.sort_unstable();
                let logical_ms = (self.current_time + 1) * MILLISECONDS_PER_SECOND;
                let anchor_ms = if due > MAX_CATCHUP_STEPS { ms }
                    else { next_tick_ms + step * MILLISECONDS_PER_SECOND };
                for index in 0..self.free_ids.len() {
                    let id = self.free_ids[index];
                    let b = self.balls.get_mut(&id).unwrap();
                    let aligning = b.mode == MODE_WARP && !b.is_warping();
                    b.evolve_behavior_for_ball(logical_ms, anchor_ms, &self.target_states, self.current_time);
                    let activated = aligning && b.is_warping();
                    self.target_states.get_mut(&id).unwrap().vel = b.vel;
                    if activated { self.stop_all_followers(id); }
                }
                collision::freeze(&mut self.collision_scratch, &self.balls);
                collision::apply(&self.collision_scratch, &mut self.balls);
                self.exited_ids.clear();
                for snapshot in self.collision_scratch.iter().filter(|snapshot| snapshot.free) {
                    let id = &snapshot.id;
                    let b = self.balls.get_mut(id).unwrap();
                    let was_warping = b.is_warping();
                    b.evolve_position(logical_ms, snapshot.time_factor);
                    if was_warping && b.mode == MODE_STOP && self.warp_exits.contains_key(id) {
                        self.exited_ids.push(*id);
                    }
                }
                for index in 0..self.exited_ids.len() {
                    let id = self.exited_ids[index];
                    if let Some(callback) = self.warp_exits.remove(&id) { callback(self); }
                }
            }
            self.current_time += 1;
        }
        if self.pilots.is_empty() {
            self.snapshot_due = 0;
        } else if self.snapshot_due != 0 && ms >= self.snapshot_due {
            self.snapshot_due = ms.saturating_add(crate::config::game().space.position_snapshot_secs.saturating_mul(MILLISECONDS_PER_SECOND));
            snapshot::schedule(self);
        }
        if self.is_cold() {
            self.pending = None;
            return;
        }
        let filetime = crate::client::filetime_from_unix_ms(ms);
        let expired: Vec<_> = self.balls.values().filter(|b|
            b.cloak != 0 && b.cloak_until > 0 && b.cloak_until <= filetime).map(|b| b.id).collect();
        for id in expired { self.uncloak(id); }
        while self.timers.first_key_value().is_some_and(|(&(due, _), _)| due <= ms) {
            let (_, timer) = self.timers.pop_first().unwrap();
            timer(self);
        }
        self.think_npcs();
        if let Some(pending) = self.pending.take() {
            for (client, events) in pending {
                if let Some((_, tx)) = self.observers.get(&client) {
                    if tx.send(events).is_err() { self.observers.remove(&client); }
                }
            }
        }
    }
    fn deliver(&mut self, about: Option<(i64, u8)>, events: Vec<Value>) {
        self.observers.retain(|&client, (ego, tx)| {
            if tx.is_closed() { return false; }
            if about.is_some_and(|(id, cloak)| cloak != 0 && id != *ego) { return true; }
            if let Some(pending) = &mut self.pending {
                pending.entry(client).or_default().extend(events.iter().cloned());
                true
            } else { tx.send(events.clone()).is_ok() }
        });
    }
    pub fn broadcast(&mut self, about: i64, events: Vec<Value>) {
        let visibility = self.ball(about).map(|b| (b.id, b.cloak));
        self.deliver(visibility, events);
    }
    pub fn add_ship(&mut self, mut ball: Ball) {
        ball.last_ms = self.current_time * MILLISECONDS_PER_SECOND;
        if ball.cloak == 0 {
            let update = add_balls_event(self.current_time, &ball);
            self.actions.push_back((self.current_time, Box::new(move |s, _| s.deliver(None, vec![update]))));
        }
        self.insert(ball);
    }
    /// Insert a neutral missile now, then launch and start its impact timer at the next destiny boundary.
    pub fn launch_missile(&mut self, ball: Ball, target: i64, launcher: i64, delay_ms: u64,
        impact: impl FnOnce(&mut System) + Send + 'static) {
        let id = ball.id;
        self.add_ship(ball);
        self.actions.push_back((self.current_time, Box::new(move |s, stamp| {
            let Some(mut ball) = s.balls.remove(&id) else { return };
            ball.last_ms = s.current_time * MILLISECONDS_PER_SECOND;
            ball.interpolation_anchor_ms = s.ms;
            let owner = launcher.checked_abs().and_then(|owner_id| {
                if owner_id == id { Some(&ball) } else { s.balls.get(&owner_id) }
            }).map(|b| (b.pos, b.vel, b.max_vel));
            if let Some(owner) = owner.filter(|_| target >= 0) {
                let target_ball = if target == launcher { None } else if target == id {
                    Some(&ball)
                } else { s.balls.get(&target) };
                let target_ball = target_ball.map(|b| (b.id, b.pos, b.radius, b.cloak));
                ball.launch_native_missile(owner, target_ball, launcher, stamp);
            }
            let visibility = (id, ball.cloak);
            s.balls.insert(id, ball);
            let update = event(stamp, "LaunchMissile", vec![
                Value::Int(id), Value::Int(target), Value::Int(launcher),
                Value::Bool(MISSILE_AIMED_LAUNCH), Value::Bool(MISSILE_MASSIVE),
            ]);
            s.deliver(Some(visibility), vec![update]);
            s.schedule(delay_ms, impact);
        })));
    }
    pub fn remove_ball(&mut self, id: i64) -> Option<Ball> {
        let b = self.take(id)?;
        self.observers.retain(|_, (ego, _)| *ego != id);
        if b.cloak == 0 {
            let update = event(self.current_time, "RemoveBalls", vec![Value::list([Value::Int(id)])]);
            self.actions.push_back((self.current_time, Box::new(move |s, _| s.deliver(None, vec![update]))));
        }
        if b.char_id.is_some() { self.registry.pump_queue(self.id, self.population()); }
        Some(b)
    }
    pub fn destroy_ball(&mut self, id: i64) -> Option<Ball> {
        let mut b = self.take(id)?;
        b.pos = b.position_at(self.ms);
        let stamp = self.current_time;
        let visibility = (b.id, b.cloak);
        let events = vec![
            event(stamp, "TerminalPlayDestructionEffect", vec![Value::Int(id), Value::Int(DESTRUCTION_EFFECT_EXPLOSION)]),
            event(stamp, "RemoveBalls", vec![Value::list([Value::Int(id)])]),
        ];
        self.actions.push_back((stamp, Box::new(move |s, _| s.deliver(Some(visibility), events))));
        if b.char_id.is_some() { self.registry.pump_queue(self.id, self.population()); }
        Some(b)
    }
    pub fn set_health(&mut self, id: i64, health: [f64; DAMAGE_LAYER_COUNT]) {
        let Some(b) = self.balls.get_mut(&id) else { return };
        b.health = health;
        let update = event(self.current_time, "OnDamageStateChange", vec![Value::Int(id), b.damage_state()]);
        self.broadcast(id, vec![update]);
    }
    pub fn uncloak(&mut self, ship: i64) -> bool {
        if self.balls.get(&ship).is_none_or(|ball| ball.cloak == 0) || !self.pending_uncloak.insert(ship) { return false; }
        self.actions.push_back((self.current_time, Box::new(move |s, stamp| s.apply_uncloak(ship, stamp))));
        true
    }
    fn apply_uncloak(&mut self, ship: i64, stamp: i64) {
        self.pending_uncloak.remove(&ship);
        let Some(ball) = self.balls.get_mut(&ship) else { return };
        if ball.cloak == 0 { return; }
        ball.cloak = 0;
        ball.cloak_until = 0;
        let add = add_balls_event(stamp, ball);
        let own = event(stamp, "UncloakBall", vec![Value::Int(ship)]);
        self.observers.retain(|&client, (ego, tx)| {
            if tx.is_closed() { return false; }
            let update = if *ego == ship { own.clone() } else { add.clone() };
            if let Some(pending) = &mut self.pending {
                pending.entry(client).or_default().push(update);
                true
            } else { tx.send(vec![update]).is_ok() }
        });
    }
    pub fn observe(&mut self, client_id: i64, ego: i64, tx: DestinyTx) -> Value {
        let system_id = self.id;
        let tick = self.current_time;
        self.observers.insert(client_id, (ego, tx));
        let mut state = vec![DESTINY_FULLSTATE];
        state.extend_from_slice(&(tick as i32).to_le_bytes());
        let mut slims = Vec::new();
        let mut damage = Vec::new();
        for b in self.balls.values().filter(|b| b.cloak == 0 || b.id == ego) {
            b.write(&mut state);
            slims.push(b.slim_item());
            if b.free {
                damage.push((Value::Int(b.id), b.damage_state()));
            }
        }
        let sol = Value::instance(
            "eve.common.script.util.slimItem.SlimItem",
            [("itemID", Value::Int(system_id)), ("typeID", Value::Int(crate::consts::TYPE_SOLAR_SYSTEM.into())), ("ownerID", Value::Int(1))],
        );
        let bag = Value::instance(
            "utillib.KeyVal",
            [
                ("state", Value::str(state)),
                ("ego", Value::Int(ego)),
                ("slims", Value::List(slims)),
                ("damageState", Value::Dict(damage)),
                ("effectStates", Value::list([])),
                ("allianceBridges", Value::list([])),
                ("droneState", Value::list([])),
                ("solItem", sol),
                ("industryLevel", Value::Int(0)),
                ("researchLevel", Value::Int(0)),
                ("dbuffState", Value::list([])),
            ],
        );

        event(tick, "SetState", vec![bag])
    }
    pub fn unobserve(&mut self, client_id: i64) { self.observers.remove(&client_id); }
    /// Queue a command for the next destiny boundary, retaining its enqueue stamp.
    pub fn command(&mut self, ship: i64, f: impl FnOnce(&mut Ball, i64, &System) -> Vec<Value> + Send + 'static) {
        self.actions.push_back((self.current_time, Box::new(move |s, stamp| {
            let Some(mut ball) = s.balls.remove(&ship) else { return };
            ball.last_ms = s.current_time * MILLISECONDS_PER_SECOND;
            let old_warp = (ball.mode == MODE_WARP).then_some(
                (ball.effect_stamp, ball.goto, ball.warp.min_range.to_bits(), ball.warp.factor));
            let events = f(&mut ball, stamp, s);
            if old_warp.is_some_and(|old| ball.mode != MODE_WARP
                || old != (ball.effect_stamp, ball.goto, ball.warp.min_range.to_bits(), ball.warp.factor)) {
                s.warp_exits.remove(&ship);
            }
            ball.interpolation_anchor_ms = s.ms;
            ball.old_pos = ball.pos;
            ball.old_vel = ball.vel;
            ball.segment_acceleration = None;
            let visibility = (ball.id, ball.cloak);
            s.balls.insert(ship, ball);
            if !events.is_empty() { s.deliver(Some(visibility), events); }
        })));
    }
}
#[cfg(test)] mod tests;
