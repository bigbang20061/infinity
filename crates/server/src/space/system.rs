//! Actor-owned ballpark state and visibility, mirroring michelle.py and destiny.
//! Celestials are supplied by the facade from SDE; dynamic state is in memory.
//! NPC decisions, spawn-group callbacks and transient timers run in the tick;
//! DB effects and changed pilot position snapshots use workers. Timers and
//! non-position dynamic state do not survive a restart.
use super::*;
use std::collections::BTreeMap;
use std::future::Future;


/// Existing server cold-skip speed tolerance.
const MOVING_SPEED_EPSILON: f64 = 1e-9;
/// Destiny destruction effect EXPLOSION.
const DESTRUCTION_EFFECT_EXPLOSION: i64 = 3;
type Timer = Box<dyn FnOnce(&mut System) + Send>;

#[derive(Default)]
pub struct System {
    pub(super) id: i64,
    balls: HashMap<i64, Ball>,
    pilots: HashMap<i64, i64>,
    observers: HashMap<i64, (i64, DestinyTx)>,
    registry: Arc<Pilots>,
    positions: HashMap<i64, V3>,
    /// Celestial warp-in points (warpin.rs), by celestial ID.
    warp_ins: HashMap<i64, V3>,
    pending: Option<HashMap<i64, Vec<Value>>>,
    timers: BTreeMap<(i64, u64), Timer>,
    timer_sequence: u64,
    pub(super) ms: i64,
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
    pub fn new(id: i64, registry: Arc<Pilots>) -> Self {
        Self { id, registry, ..Self::default() }
    }
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
    pub(super) fn set_warp_in(&mut self, id: i64, point: V3) { self.warp_ins.insert(id, point); }
    pub fn ball(&self, id: i64) -> Option<&Ball> {
        self.balls.get(&id)
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

    pub fn insert(&mut self, ball: Ball) {
        self.take(ball.id);
        if let Some(char_id) = ball.char_id {
            self.pilots.insert(char_id, ball.id);
            self.snapshot_generation = self.snapshot_generation.checked_add(1).expect("snapshot generation exhausted");
            self.snapshot_positions.insert(ball.id, (self.snapshot_generation, ball.pos));
            if self.snapshot_due == 0 {
                self.snapshot_due = self.ms.saturating_add(snapshot::POSITION_SNAPSHOT_INTERVAL_MS as i64);
            }
        }
        self.balls.insert(ball.id, ball);
    }

    pub fn take(&mut self, id: i64) -> Option<Ball> {
        let ball = self.balls.remove(&id)?;
        self.snapshot_positions.remove(&id);
        if let Some(char_id) = ball.char_id {
            if self.pilots.get(&char_id) == Some(&id) {
                self.pilots.remove(&char_id);
            }
        }
        self.npc_removed(id);
        Some(ball)
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
    pub(super) fn evolve_all(&mut self, ms: i64) {
        self.ms = ms;
        self.positions.clear();
        self.positions.extend(self.balls.iter().map(|(&id, b)| (id, b.pos)));
        for b in self.balls.values_mut() { b.evolve(ms, &self.positions); }
    }
    pub(super) fn is_cold(&self) -> bool {
        !self.timers.first_key_value().is_some_and(|(&(due, _), _)| due <= self.ms)
            && self.brains.is_empty() && self.observers.is_empty() && !self.balls.values().any(|b|
            (b.free && b.mode != MODE_MISSILE && (b.mode != MODE_STOP || len(b.vel) > MOVING_SPEED_EPSILON))
            || (b.cloak != 0 && b.cloak_until > 0))
    }
    pub fn tick(&mut self, ms: i64) {
        self.observers.retain(|_, (_, tx)| !tx.is_closed());
        self.ms = ms;
        if self.pilots.is_empty() {
            self.snapshot_due = 0;
        } else if self.snapshot_due != 0 && ms >= self.snapshot_due {
            self.snapshot_due = ms.saturating_add(snapshot::POSITION_SNAPSHOT_INTERVAL_MS as i64);
            snapshot::schedule(self);
        }
        if self.is_cold() { return; }
        self.pending = Some(HashMap::new());
        self.evolve_all(ms);
        let filetime = crate::client::filetime_from_unix_ms(ms);
        let expired: Vec<_> = self.balls.values().filter(|b|
            b.cloak != 0 && b.cloak_until > 0 && b.cloak_until <= filetime).map(|b| b.id).collect();
        for id in expired { self.uncloak(id); }
        while self.timers.first_key_value().is_some_and(|(&(due, _), _)| due <= ms) {
            let (_, timer) = self.timers.pop_first().unwrap();
            timer(self);
        }
        self.think_npcs();
        for (client, events) in self.pending.take().unwrap() {
            if let Some((_, tx)) = self.observers.get(&client) {
                if tx.send(events).is_err() { self.observers.remove(&client); }
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
    pub fn add_ship(&mut self, ball: Ball) {
        if ball.cloak == 0 { self.deliver(None, vec![add_balls_event(destiny_stamp(self.ms), &ball)]); }
        self.insert(ball);
    }
    pub fn remove_ball(&mut self, id: i64) -> Option<Ball> {
        let b = self.take(id)?;
        self.observers.retain(|_, (ego, _)| *ego != id);
        if b.cloak == 0 {
            self.deliver(None, vec![event(destiny_stamp(self.ms), "RemoveBalls", vec![Value::list([Value::Int(id)])])]);
        }
        if b.char_id.is_some() { self.registry.pump_queue(self.id, self.population()); }
        Some(b)
    }
    pub fn destroy_ball(&mut self, id: i64) -> Option<Ball> {
        let b = self.take(id)?;
        let stamp = destiny_stamp(self.ms);
        self.deliver(Some((b.id, b.cloak)), vec![
            event(stamp, "TerminalPlayDestructionEffect", vec![Value::Int(id), Value::Int(DESTRUCTION_EFFECT_EXPLOSION)]),
            event(stamp, "RemoveBalls", vec![Value::list([Value::Int(id)])]),
        ]);
        if b.char_id.is_some() { self.registry.pump_queue(self.id, self.population()); }
        Some(b)
    }
    pub fn set_health(&mut self, id: i64, health: [f64; DAMAGE_LAYER_COUNT]) {
        let Some(b) = self.balls.get_mut(&id) else { return };
        b.health = health;
        let update = event(destiny_stamp(self.ms), "OnDamageStateChange", vec![Value::Int(id), b.damage_state()]);
        self.broadcast(id, vec![update]);
    }
    pub fn uncloak(&mut self, ship: i64) -> bool {
        let Some(ball) = self.balls.get_mut(&ship) else { return false };
        if ball.cloak == 0 { return false; }
        ball.cloak = 0;
        ball.cloak_until = 0;
        let add = add_balls_event(destiny_stamp(self.ms), ball);
        let own = event(destiny_stamp(self.ms), "UncloakBall", vec![Value::Int(ship)]);
        self.observers.retain(|&client, (ego, tx)| {
            if tx.is_closed() { return false; }
            let update = if *ego == ship { own.clone() } else { add.clone() };
            if let Some(pending) = &mut self.pending {
                pending.entry(client).or_default().push(update);
                true
            } else { tx.send(vec![update]).is_ok() }
        });
        true
    }
    pub fn observe(&mut self, client_id: i64, ego: i64, tx: DestinyTx) -> Value {
        let system_id = self.id;
        let tick = destiny_stamp(self.ms);
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
    /// The commanded ball is detached temporarily; the rest of the system is borrowed, not cloned.
    pub fn command(&mut self, ship: i64, f: impl FnOnce(&mut Ball, i64, &System) -> Vec<Value>) {
        let Some(mut ball) = self.balls.remove(&ship) else { return };
        let events = f(&mut ball, destiny_stamp(self.ms), self);
        let visibility = (ball.id, ball.cloak);
        self.balls.insert(ship, ball);
        if !events.is_empty() { self.deliver(Some(visibility), events); }
    }
}
#[cfg(test)] mod tests;
