//! Solar systems in space: a server-side ballpark per system, the destiny
//! binary state format (carbonengine destiny Thunkers.cpp WriteBallToStream),
//! and destiny event broadcasting (michelle.py DoDestinyUpdate).
//!
//! The client runs the same destiny simulation locally from the events we
//! send; the server integrates ships with destiny's model (exponential
//! approach to target velocity with tau = mass * agility / 1e6, warp phases
//! from Ballpark.cpp SetupWarpConstants) to know where things are.

use crate::db::Db;
use anyhow::Result;
use marshal::{DbRowDescriptor, DbType, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tracing::info;

// DSTBALLMODE
pub const MODE_GOTO: u8 = 0;
pub const MODE_FOLLOW: u8 = 1;
pub const MODE_STOP: u8 = 2;
pub const MODE_WARP: u8 = 3;
pub const MODE_ORBIT: u8 = 4;
pub const MODE_MISSILE: u8 = 5;
pub const MODE_RIGID: u8 = 11;
// BALLFLAGS
const FLAG_FREE: u8 = 0x01;
const FLAG_GLOBAL: u8 = 0x02;
const FLAG_MASSIVE: u8 = 0x04;
const FLAG_INTERACTIVE: u8 = 0x08;
// DSTCLOAKMODE (IDstConstants.h): the session-change cloak a ship arrives with.
pub const CLOAK_RESTORE: u8 = 2;
const DESTINY_FULLSTATE: u8 = 0;
const DESTINY_BALLS: u8 = 1;

const AU: f64 = 0.1495978707e12;
const WARP_FACTOR_TO_AU_PER_SECOND: f64 = 0.001;
const WARP_FACTOR_TO_ACCELERATION: f64 = 1.0 / 1000.0;
const WARP_FACTOR_TO_DECELERATION: f64 = 1.0 / 3000.0;

pub type V3 = [f64; 3];

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn scale(a: V3, s: f64) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}
fn len(a: V3) -> f64 {
    (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
}
fn norm(a: V3) -> V3 {
    let l = len(a);
    if l < 1e-9 {
        [1.0, 0.0, 0.0]
    } else {
        scale(a, 1.0 / l)
    }
}

/// Destiny tick counter: one per simulated second.
pub fn now_tick() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64
}

#[derive(Clone)]
pub struct Warp {
    pub from: V3,
    pub to: V3,
    pub start: i64,
    pub duration: f64,
    pub factor: i64,
    pub min_range: f64,
}

#[derive(Clone)]
pub struct Ball {
    pub id: i64,
    pub type_id: i32,
    pub owner_id: i64,
    pub corp_id: i64,
    pub alliance_id: i64,
    pub char_id: Option<i64>,
    pub mode: u8,
    pub radius: f32,
    pub mass: f64,
    pub max_vel: f32,
    pub agility: f32,
    pub speed_fraction: f32,
    pub pos: V3,
    pub vel: V3,
    pub goto: V3,
    pub follow_id: i64,
    pub follow_range: f32,
    pub free: bool,
    pub global: bool,
    pub massive: bool,
    pub warp: Option<Warp>,
    pub warp_factor: i64,
    pub last_tick: i64,
    /// Cloak mode (0 = visible). Cloaked balls exist only for their owner.
    pub cloak: u8,
    /// When an arrival cloak lapses (FILETIME), to tell a lapse from a later cloak.
    pub cloak_until: i64,
    /// Shield, armor and structure left, as fractions (damage state).
    pub health: [f64; 3],
    /// For missiles: the ship that launched it (destiny's mOwnerId).
    pub launcher: i64,
    /// Extra slim item fields (jumps for gates, names...).
    pub slim: Vec<(&'static str, Value)>,
}

impl Ball {
    fn flags(&self) -> u8 {
        (if self.free { FLAG_FREE } else { 0 })
            | (if self.global { FLAG_GLOBAL } else { 0 })
            | (if self.massive { FLAG_MASSIVE } else { 0 })
            | if self.free { FLAG_INTERACTIVE } else { 0 }
    }

    /// Ballpark::WriteBallToStream (no miniballs, no dynamical orientation).
    fn write(&self, o: &mut Vec<u8>) {
        o.extend_from_slice(&self.id.to_le_bytes());
        o.push(self.mode);
        o.extend_from_slice(&self.radius.to_le_bytes());
        for c in self.pos {
            o.extend_from_slice(&c.to_le_bytes());
        }
        o.push(self.flags());
        if self.mode != MODE_RIGID {
            o.extend_from_slice(&self.mass.to_le_bytes());
            o.push(self.cloak); // isCloaked
            o.extend_from_slice(&(-1i64).to_le_bytes()); // harmonic
            o.extend_from_slice(&(self.corp_id as i32).to_le_bytes());
            o.extend_from_slice(&(self.alliance_id as i32).to_le_bytes());
        }
        if self.free {
            o.extend_from_slice(&self.max_vel.to_le_bytes());
            for c in self.vel {
                o.extend_from_slice(&c.to_le_bytes());
            }
            o.extend_from_slice(&self.agility.to_le_bytes());
            o.extend_from_slice(&self.speed_fraction.to_le_bytes());
        }
        o.push(0xFF); // formationID -1
        match self.mode {
            MODE_FOLLOW | MODE_ORBIT => {
                o.extend_from_slice(&self.follow_id.to_le_bytes());
                o.extend_from_slice(&self.follow_range.to_le_bytes());
            }
            MODE_GOTO => {
                for c in self.goto {
                    o.extend_from_slice(&c.to_le_bytes());
                }
            }
            MODE_MISSILE => {
                o.extend_from_slice(&self.follow_id.to_le_bytes());
                o.extend_from_slice(&self.follow_range.to_le_bytes());
                o.extend_from_slice(&self.launcher.to_le_bytes());
                o.extend_from_slice(&(self.last_tick as i32).to_le_bytes()); // effectStamp: launch tick
                for c in self.goto {
                    o.extend_from_slice(&c.to_le_bytes());
                }
            }
            MODE_WARP => {
                let w = self.warp.clone().unwrap_or(Warp { from: self.pos, to: self.goto, start: now_tick(), duration: 0.0, factor: 3000, min_range: 0.0 });
                for c in w.to {
                    o.extend_from_slice(&c.to_le_bytes());
                }
                o.extend_from_slice(&(w.start as i32).to_le_bytes()); // effectStamp
                o.extend_from_slice(&len(sub(w.to, w.from)).to_le_bytes()); // total warp length
                o.extend_from_slice(&(w.min_range as i64).to_le_bytes());
                o.extend_from_slice(&w.factor.to_le_bytes());
            }
            _ => {}
        }
    }

    /// `((shield, shieldRechargeTau, time), armor, structure)` (spaceObject.OnDamageState).
    pub fn damage_state(&self) -> Value {
        let [shield, armor, hull] = self.health;
        Value::tuple([
            Value::tuple([Value::Float(shield), Value::Float(100000.0), Value::Int(crate::client::filetime_now())]),
            Value::Float(armor),
            Value::Float(hull),
        ])
    }

    fn slim_item(&self) -> Value {
        let mut fields: Vec<(&str, Value)> = vec![
            ("itemID", Value::Int(self.id)),
            ("typeID", Value::Int(self.type_id as i64)),
            ("ownerID", Value::Int(self.owner_id)),
        ];
        if let Some(ch) = self.char_id {
            fields.push(("charID", Value::Int(ch)));
            fields.push(("corpID", Value::Int(self.corp_id)));
            fields.push(("allianceID", if self.alliance_id > 0 { Value::Int(self.alliance_id) } else { Value::None }));
            fields.push(("warFactionID", Value::None));
            if !self.slim.iter().any(|(k, _)| *k == "securityStatus") {
                fields.push(("securityStatus", Value::Float(0.0)));
            }
            fields.push(("bounty", Value::Int(0)));
        }
        fields.extend(self.slim.iter().cloned());
        Value::instance("eve.common.script.util.slimItem.SlimItem", fields)
    }

    /// Advance free balls to `tick` (server-side approximation of Ballpark::Evolve).
    fn evolve(&mut self, tick: i64, others: &HashMap<i64, V3>) {
        // Missiles fly client-side; the server only times their impact, and their
        // last_tick stays the launch tick (the stream's effectStamp).
        if self.mode == MODE_MISSILE {
            return;
        }
        if !self.free || tick <= self.last_tick {
            self.last_tick = self.last_tick.max(tick);
            return;
        }
        if let Some(w) = self.warp.clone() {
            let t = (tick - w.start) as f64;
            if t >= w.duration {
                self.pos = w.to;
                self.vel = [0.0; 3];
                self.warp = None;
                self.mode = MODE_STOP;
                self.speed_fraction = 0.0;
            } else {
                let frac = (t / w.duration.max(1.0)).clamp(0.0, 1.0);
                self.pos = add(w.from, scale(sub(w.to, w.from), frac));
            }
            self.last_tick = tick;
            return;
        }
        let tau = (self.mass * self.agility as f64 / 1e6).max(0.1);
        for _ in self.last_tick..tick {
            let target_speed = (self.max_vel * self.speed_fraction) as f64;
            let target_vel = match self.mode {
                MODE_GOTO => scale(norm(self.goto), target_speed),
                MODE_FOLLOW | MODE_ORBIT => match others.get(&self.follow_id) {
                    Some(&p) => {
                        let d = sub(p, self.pos);
                        if len(d) <= self.follow_range as f64 + self.radius as f64 {
                            [0.0; 3]
                        } else {
                            scale(norm(d), target_speed)
                        }
                    }
                    None => [0.0; 3],
                },
                _ => [0.0; 3],
            };
            let k = (-1.0 / tau).exp();
            let new_vel = add(target_vel, scale(sub(self.vel, target_vel), k));
            self.pos = add(self.pos, scale(add(self.vel, new_vel), 0.5));
            self.vel = new_vel;
        }
        self.last_tick = tick;
    }
}

/// Ballpark::SetupWarpConstants -> total warp duration in seconds.
pub fn warp_duration(factor: f64, distance: f64) -> f64 {
    let accel = factor * WARP_FACTOR_TO_ACCELERATION;
    let decel = (factor * WARP_FACTOR_TO_DECELERATION).min(2.0);
    let speed = (factor * WARP_FACTOR_TO_AU_PER_SECOND * AU).min((distance + 1.0) * accel * decel / (accel + decel));
    let accel_t = (speed / accel).ln() / accel;
    let cruise_t = (distance / speed) - (1.0 / accel) - (1.0 / decel) + (1.0 / speed);
    let decel_t = (speed / decel).ln() / decel;
    (accel_t + cruise_t.max(0.0) + decel_t).max(1.0) + 1.0
}

pub type DestinyTx = mpsc::UnboundedSender<Vec<Value>>;
/// A client's notification channel: (event, args) scattered on its client.
pub type NotifyTx = mpsc::UnboundedSender<(String, Vec<Value>)>;

/// Jump keys from the jump queue stay valid this long (s).
pub const JUMP_KEY_LIFE: i64 = 60;
const SEC: i64 = 10_000_000;

/// Pilots waiting to jump into a full system, and the entry keys already issued.
#[derive(Default)]
struct JumpQueue {
    waiting: Vec<i64>,
    /// charID -> FILETIME the key was issued
    keys: HashMap<i64, i64>,
}

/// AddBalls for one ball.
fn add_balls_event(tick: i64, ball: &Ball) -> Value {
    let mut state = vec![DESTINY_BALLS];
    state.extend_from_slice(&(tick as i32).to_le_bytes());
    ball.write(&mut state);
    let chunk = Value::tuple([Value::str(state), Value::list([ball.slim_item()]), Value::Dict(vec![])]);
    event(tick, "AddBalls", vec![chunk])
}

struct System {
    balls: HashMap<i64, Ball>,
    /// clientID -> (ego ship, push channel)
    observers: HashMap<i64, (i64, DestinyTx)>,
}

impl System {
    fn positions(&self) -> HashMap<i64, V3> {
        self.balls.iter().map(|(k, b)| (*k, b.pos)).collect()
    }
    fn evolve_all(&mut self, tick: i64) {
        let pos = self.positions();
        for b in self.balls.values_mut() {
            b.evolve(tick, &pos);
        }
    }
    fn broadcast(&self, events: Vec<Value>) {
        for (_, tx) in self.observers.values() {
            let _ = tx.send(events.clone());
        }
    }
    /// Events about `ball`: everyone sees them, unless it is cloaked (then only its pilot).
    fn broadcast_about(&self, ball: &Ball, events: Vec<Value>) {
        if ball.cloak == 0 {
            return self.broadcast(events);
        }
        for (ego, tx) in self.observers.values() {
            if *ego == ball.id {
                let _ = tx.send(events.clone());
            }
        }
    }
}

/// `(stamp, (funcName, args))`
pub fn event(tick: i64, name: &str, args: Vec<Value>) -> Value {
    Value::tuple([Value::Int(tick), Value::tuple([Value::str(name), Value::Tuple(args)])])
}

pub struct World {
    db: Db,
    systems: Mutex<HashMap<i64, System>>,
    /// Pilots allowed in space per solar system before jumps queue (0 = unlimited).
    capacity: usize,
    queues: Mutex<HashMap<i64, JumpQueue>>,
    /// charID -> notification channel of the client playing it.
    pilots: Mutex<HashMap<i64, NotifyTx>>,
}

impl World {
    pub fn new(db: Db, capacity: usize) -> Arc<World> {
        Arc::new(World { db, systems: Mutex::new(HashMap::new()), capacity, queues: Default::default(), pilots: Default::default() })
    }

    pub fn register_pilot(&self, char_id: i64, tx: NotifyTx) {
        self.pilots.lock().unwrap().insert(char_id, tx);
    }

    pub fn unregister_pilot(&self, char_id: i64) {
        self.pilots.lock().unwrap().remove(&char_id);
        for q in self.queues.lock().unwrap().values_mut() {
            q.waiting.retain(|c| *c != char_id);
        }
    }

    pub fn notify_pilot(&self, char_id: i64, event: &str, args: Vec<Value>) {
        if let Some(tx) = self.pilots.lock().unwrap().get(&char_id) {
            let _ = tx.send((event.to_string(), args));
        }
    }

    /// Pilots in space in `system_id`.
    fn population(&self, system_id: i64) -> usize {
        self.systems.lock().unwrap().get(&system_id).map(|s| s.balls.values().filter(|b| b.char_id.is_some()).count()).unwrap_or(0)
    }

    /// May `char_id` jump into `system_id` now? `Err(position)` puts them in its jump queue
    /// (jumpQueue.py); a key issued from the queue lets them in within JUMP_KEY_LIFE.
    pub fn request_entry(&self, system_id: i64, char_id: i64) -> Result<(), i64> {
        if self.capacity == 0 {
            return Ok(());
        }
        let population = self.population(system_id);
        {
            let mut queues = self.queues.lock().unwrap();
            let q = queues.entry(system_id).or_default();
            let now = crate::client::filetime_now();
            q.keys.retain(|_, issued| now - *issued < JUMP_KEY_LIFE * SEC);
            if q.keys.remove(&char_id).is_some() {
                q.waiting.retain(|c| *c != char_id);
                return Ok(());
            }
            if q.waiting.is_empty() && population + q.keys.len() < self.capacity {
                return Ok(());
            }
            if !q.waiting.contains(&char_id) {
                q.waiting.push(char_id);
            }
        }
        self.pump_queue(system_id);
        let queues = self.queues.lock().unwrap();
        let Some(q) = queues.get(&system_id) else { return Err(1) };
        if q.keys.contains_key(&char_id) {
            // Room opened while queueing: the key is theirs, the next attempt goes through.
            return Err(0);
        }
        let space = self.capacity.saturating_sub(population + q.keys.len()) as i64;
        let idx = q.waiting.iter().position(|c| *c == char_id).unwrap_or(0) as i64;
        Err((idx + 1 - space).max(1))
    }

    /// Issue entry keys for free room in `system_id` and tell everyone queued for it.
    pub fn pump_queue(&self, system_id: i64) {
        if self.capacity == 0 {
            return;
        }
        let population = self.population(system_id);
        let (update, recipients) = {
            let mut queues = self.queues.lock().unwrap();
            let Some(q) = queues.get_mut(&system_id) else { return };
            let now = crate::client::filetime_now();
            q.keys.retain(|_, issued| now - *issued < JUMP_KEY_LIFE * SEC);
            while !q.waiting.is_empty() && population + q.keys.len() < self.capacity {
                let next = q.waiting.remove(0);
                q.keys.insert(next, now);
            }
            let space = self.capacity.saturating_sub(population + q.keys.len()) as i64;
            // OnJumpQueueUpdate(solarsystemID, space, queue, jumpKeys, keyLife)
            let update = vec![
                Value::Int(system_id),
                Value::Int(space),
                Value::list(q.waiting.iter().map(|c| Value::Int(*c))),
                Value::Dict(q.keys.iter().map(|(c, t)| (Value::Int(*c), Value::Int(*t))).collect()),
                Value::Int(JUMP_KEY_LIFE),
            ];
            let recipients: Vec<i64> = q.waiting.iter().chain(q.keys.keys()).copied().collect();
            (update, recipients)
        };
        for char_id in recipients {
            self.notify_pilot(char_id, "OnJumpQueueUpdate", update.clone());
        }
    }

    /// Load a solar system's celestials (once).
    async fn ensure_loaded(&self, system_id: i64) -> Result<()> {
        if self.systems.lock().unwrap().contains_key(&system_id) {
            return Ok(());
        }
        let rows = self
            .db
            .client
            .query(
                "SELECT c.item_id, c.type_id, COALESCE(c.radius, t.radius, 1000)::float8, c.x, c.y, c.z,
                        COALESCE(s.owner_id, 1)::bigint, c.group_id::bigint,
                        g.dest_stargate_id::bigint, g.dest_solar_system_id::bigint
                 FROM sde.map_celestials c JOIN sde.inv_types t USING (type_id)
                 LEFT JOIN sde.sta_stations s ON s.station_id = c.item_id
                 LEFT JOIN sde.map_stargates g ON g.stargate_id = c.item_id
                 WHERE c.solar_system_id = $1",
                &[&(system_id as i32)],
            )
            .await?;
        let tick = now_tick();
        let mut balls = HashMap::new();
        for r in rows {
            let id: i64 = r.get(0);
            let mut slim = Vec::new();
            if let (Some(dest_gate), Some(dest_sys)) = (r.get::<_, Option<i64>>(8), r.get::<_, Option<i64>>(9)) {
                // A row: unpacked as (destGate, destSystem) by subwaySvc.GetPriorityLoads,
                // read as .toCelestialID / .locationID elsewhere.
                let jump = DbRowDescriptor::new([("toCelestialID", DbType::I4), ("locationID", DbType::I4)])
                    .row(&[Value::Int(dest_gate), Value::Int(dest_sys)]);
                slim.push(("jumps", Value::list([jump])));
                // clientControllerRegistry.StargateSetup indexes these (stage, maximum) pairs.
                let pair = || Value::tuple([Value::Int(0), Value::Int(1)]);
                slim.push(("destinationCorruptionStageAndMaximum", pair()));
                slim.push(("localCorruptionStageAndMaximum", pair()));
                slim.push(("destinationSuppressionStageAndMaximum", pair()));
                slim.push(("localSuppressionStageAndMaximum", pair()));
            }
            balls.insert(
                id,
                Ball {
                    id,
                    type_id: r.get::<_, Option<i32>>(1).unwrap_or(0),
                    owner_id: r.get(6),
                    corp_id: r.get(6),
                    alliance_id: -1,
                    char_id: None,
                    mode: MODE_RIGID,
                    radius: r.get::<_, f64>(2) as f32,
                    mass: 1e34,
                    max_vel: 0.0,
                    agility: 1.0,
                    speed_fraction: 0.0,
                    pos: [r.get(3), r.get(4), r.get(5)],
                    vel: [0.0; 3],
                    goto: [0.0; 3],
                    follow_id: 0,
                    follow_range: 0.0,
                    free: false,
                    global: true,
                    massive: true,
                    warp: None,
                    warp_factor: 0,
                    last_tick: tick,
                    cloak: 0,
                    cloak_until: 0,
                    health: [1.0; 3],
                    launcher: 0,
                    slim,
                },
            );
        }
        info!("loaded solar system {system_id} with {} celestials", balls.len());
        self.systems.lock().unwrap().entry(system_id).or_insert(System { balls, observers: HashMap::new() });
        Ok(())
    }

    /// Every ball in `system_id`, evolved to now.
    pub fn balls_snapshot(&self, system_id: i64) -> Option<HashMap<i64, Ball>> {
        let mut s = self.systems.lock().unwrap();
        let sys = s.get_mut(&system_id)?;
        sys.evolve_all(now_tick());
        Some(sys.balls.clone())
    }

    pub async fn ball(&self, system_id: i64, id: i64) -> Option<Ball> {
        let mut s = self.systems.lock().unwrap();
        let sys = s.get_mut(&system_id)?;
        sys.evolve_all(now_tick());
        sys.balls.get(&id).cloned()
    }

    /// Put a ship ball into a system and tell everyone already there.
    pub async fn add_ship(&self, system_id: i64, ball: Ball) -> Result<()> {
        self.ensure_loaded(system_id).await?;
        let mut s = self.systems.lock().unwrap();
        let sys = s.get_mut(&system_id).unwrap();
        let tick = now_tick();
        sys.evolve_all(tick);
        // A cloaked arrival is only for its pilot, who gets it with SetState.
        if ball.cloak == 0 {
            sys.broadcast(vec![add_balls_event(tick, &ball)]);
        }
        sys.balls.insert(ball.id, ball);
        Ok(())
    }

    /// Remove a ball (docking, jumping, logoff) and tell observers.
    pub fn remove_ball(&self, system_id: i64, id: i64) -> Option<Ball> {
        let b = {
            let mut s = self.systems.lock().unwrap();
            let sys = s.get_mut(&system_id)?;
            sys.evolve_all(now_tick());
            let b = sys.balls.remove(&id);
            sys.observers.retain(|_, (ego, _)| *ego != id);
            if b.as_ref().is_some_and(|b| b.cloak == 0) {
                sys.broadcast(vec![event(now_tick(), "RemoveBalls", vec![Value::list([Value::Int(id)])])]);
            }
            b
        };
        // Someone left: room for whoever is queued to jump in.
        self.pump_queue(system_id);
        b
    }

    /// Blow a ball up: everyone sees it explode (TerminalPlayDestructionEffect with
    /// destructionType.EXPLOSION, in the same batch as its RemoveBalls; michelle.DoDestinyUpdate).
    pub fn destroy_ball(&self, system_id: i64, id: i64) -> Option<Ball> {
        let mut s = self.systems.lock().unwrap();
        let sys = s.get_mut(&system_id)?;
        let tick = now_tick();
        sys.evolve_all(tick);
        let b = sys.balls.remove(&id)?;
        sys.broadcast(vec![
            event(tick, "TerminalPlayDestructionEffect", vec![Value::Int(id), Value::Int(3)]),
            event(tick, "RemoveBalls", vec![Value::list([Value::Int(id)])]),
        ]);
        Some(b)
    }

    /// Update a ball's damage state and tell everyone who can see it.
    pub fn set_health(&self, system_id: i64, id: i64, health: [f64; 3]) {
        let mut s = self.systems.lock().unwrap();
        let Some(sys) = s.get_mut(&system_id) else { return };
        let Some(b) = sys.balls.get_mut(&id) else { return };
        b.health = health;
        let b = b.clone();
        sys.broadcast_about(&b, vec![event(now_tick(), "OnDamageStateChange", vec![Value::Int(id), b.damage_state()])]);
    }

    /// Broadcast destiny events about `about` (special effects...) without changing it.
    pub fn broadcast(&self, system_id: i64, about: i64, events: Vec<Value>) {
        let s = self.systems.lock().unwrap();
        let Some(sys) = s.get(&system_id) else { return };
        match sys.balls.get(&about) {
            Some(b) => sys.broadcast_about(b, events),
            None => sys.broadcast(events),
        }
    }

    /// Drop `ship`'s cloak: its pilot's park uncloaks it, everyone else has it added.
    /// Returns whether it was cloaked.
    pub fn uncloak(&self, system_id: i64, ship: i64) -> bool {
        let mut s = self.systems.lock().unwrap();
        let Some(sys) = s.get_mut(&system_id) else { return false };
        let tick = now_tick();
        sys.evolve_all(tick);
        let Some(ball) = sys.balls.get_mut(&ship) else { return false };
        if ball.cloak == 0 {
            return false;
        }
        ball.cloak = 0;
        let add = add_balls_event(tick, ball);
        let own = event(tick, "UncloakBall", vec![Value::Int(ship)]);
        for (ego, tx) in sys.observers.values() {
            let _ = tx.send(vec![if *ego == ship { own.clone() } else { add.clone() }]);
        }
        true
    }

    /// Lapse an arrival cloak set to end at `until`, unless it was already dropped or renewed.
    pub fn expire_cloak(&self, system_id: i64, ship: i64, until: i64) -> bool {
        let current = self.systems.lock().unwrap().get(&system_id).and_then(|s| s.balls.get(&ship)).map(|b| (b.cloak, b.cloak_until));
        match current {
            Some((cloak, end)) if cloak != 0 && end == until => self.uncloak(system_id, ship),
            _ => false,
        }
    }

    /// Register a client as watching `system_id` with `ego` as its ship and
    /// return the SetState event for it.
    pub async fn observe(&self, system_id: i64, client_id: i64, ego: i64, tx: DestinyTx) -> Result<Value> {
        self.ensure_loaded(system_id).await?;
        let mut s = self.systems.lock().unwrap();
        let sys = s.get_mut(&system_id).unwrap();
        let tick = now_tick();
        sys.evolve_all(tick);
        sys.observers.insert(client_id, (ego, tx));
        let mut state = vec![DESTINY_FULLSTATE];
        state.extend_from_slice(&(tick as i32).to_le_bytes());
        let mut slims = Vec::new();
        let mut damage = Vec::new();
        for b in sys.balls.values().filter(|b| b.cloak == 0 || b.id == ego) {
            b.write(&mut state);
            slims.push(b.slim_item());
            if b.free {
                damage.push((Value::Int(b.id), b.damage_state()));
            }
        }
        let sol = Value::instance(
            "eve.common.script.util.slimItem.SlimItem",
            [("itemID", Value::Int(system_id)), ("typeID", Value::Int(5)), ("ownerID", Value::Int(1))],
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
        Ok(event(tick, "SetState", vec![bag]))
    }

    pub fn unobserve(&self, client_id: i64) {
        for sys in self.systems.lock().unwrap().values_mut() {
            sys.observers.remove(&client_id);
        }
    }

    /// Apply a movement command to `ship` and broadcast the matching destiny event(s).
    pub fn command(&self, system_id: i64, ship: i64, f: impl FnOnce(&mut Ball, i64, &HashMap<i64, Ball>) -> Vec<Value>) -> Option<Ball> {
        let mut s = self.systems.lock().unwrap();
        let sys = s.get_mut(&system_id)?;
        let tick = now_tick();
        sys.evolve_all(tick);
        let others = sys.balls.clone();
        let ball = sys.balls.get_mut(&ship)?;
        let events = f(ball, tick, &others);
        let snapshot = ball.clone();
        if !events.is_empty() {
            sys.broadcast_about(&snapshot, events);
        }
        Some(snapshot)
    }
}
