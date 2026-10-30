//! Solar systems in space: a server-side ballpark per system, the destiny
//! binary state format (carbonengine destiny Thunkers.cpp WriteBallToStream),
//! and destiny event broadcasting (michelle.py DoDestinyUpdate).
//!
//! The client runs the same destiny simulation locally from the events we
//! send; the server integrates ships with destiny's model (exponential
//! approach to target velocity with tau = mass * agility / 1e6, warp phases
//! from Ballpark.cpp SetupWarpConstants) to know where things are.
//!
//! Celestials come from the SDE; dynamic balls stay in memory. Each system
//! owns an actor with a 100 ms tick and closure messages. Destiny stamps remain
//! whole seconds. NPC decisions, spawn groups and closure timers live in the actor;
//! NPC combat effects run sequentially and deferred jobs concurrently outside it.
//! Timers fire on ticks in deadline and scheduling order; timers are not persistent.
//! Changed pilot positions are periodically saved to game.items and flushed at shutdown.
//! Other dynamic ball state is not persisted here.
//! Shared vector distances are defined here for simulation and service callers.

use crate::consts::{AU, DAMAGE_LAYER_COUNT, FILETIME_SECOND, SECONDS_PER_MINUTE};
use crate::db::Db;
use anyhow::Result;
use marshal::{DbRowDescriptor, DbType, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tracing::info;

mod actor;
mod system;
mod npc;
mod warpin;
mod snapshot;
pub use npc::{HOSTILE_TO_ALL, NpcAction, NpcBrain, NpcConfig, NpcEffects, NpcFuture};
pub use system::System;
use actor::SystemHandle;

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

/// SI time conversion.
const MILLISECONDS_PER_SECOND: i64 = 1000;
/// SI distance conversion.
const METRES_PER_KILOMETRE: f64 = 1000.0;
/// SI mass conversion: kilograms per tonne.
const KILOGRAMS_PER_TONNE: f64 = 1000.0;
/// Existing normalization tolerance.
const VECTOR_LENGTH_EPSILON: f64 = 1e-9;
/// Existing player targeting formula, preserved by D17.
const LOCK_TIME_FACTOR_SECONDS: f64 = 40_000.0;
/// Existing player targeting minimum.
const MIN_LOCK_TIME_SECONDS: f64 = 0.5;
/// Existing player targeting maximum.
const MAX_LOCK_TIME_SECONDS: f64 = 60.0;
/// Existing missing-warp-state serialization default.
const DEFAULT_WARP_FACTOR: i64 = 3000;
/// Existing server damage-state placeholder policy.
const DAMAGE_STATE_SHIELD_RECHARGE_TAU: f64 = 100_000.0;
/// Existing destiny mass/agility integration scale.
const MASS_AGILITY_SCALE: f64 = 1e6;
/// Existing server integration floor.
const MIN_MOVEMENT_TAU_SECONDS: f64 = 0.1;
/// Carbonengine Ballpark::SetupWarpConstants.
const MAX_WARP_DECELERATION: f64 = 2.0;
/// Existing server fallback for missing celestial radii.
const CELESTIAL_FALLBACK_RADIUS: f64 = 1000.0;
/// Existing server rigid-celestial representation.
const CELESTIAL_STATIC_MASS: f64 = 1e34;

/// Carbonengine warp factor to AU per second.
const WARP_FACTOR_TO_AU_PER_SECOND: f64 = 0.001;
/// Carbonengine warp factor to acceleration.
const WARP_FACTOR_TO_ACCELERATION: f64 = 1.0 / 1000.0;
/// Carbonengine warp factor to deceleration.
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
/// Euclidean distance between two positions.
pub fn dist(a: V3, b: V3) -> f64 {
    len(sub(a, b))
}
fn norm(a: V3) -> V3 {
    let l = len(a);
    if l < VECTOR_LENGTH_EPSILON {
        [1.0, 0.0, 0.0]
    } else {
        scale(a, 1.0 / l)
    }
}

/// Convert Unix milliseconds to a whole-second destiny stamp.
pub fn destiny_stamp(unix_ms: i64) -> i64 { unix_ms / MILLISECONDS_PER_SECOND }
/// Convert milliseconds to seconds.
pub fn ms_to_seconds(ms: f64) -> f64 { ms / MILLISECONDS_PER_SECOND as f64 }
/// Convert minutes to seconds without discarding fractional minutes.
pub fn minutes_to_seconds(minutes: f64) -> f64 { minutes * SECONDS_PER_MINUTE as f64 }
/// Convert seconds to milliseconds without discarding fractions.
pub fn seconds_to_ms_f64(seconds: f64) -> f64 { seconds * MILLISECONDS_PER_SECOND as f64 }
/// Convert seconds to whole milliseconds.
pub fn seconds_to_ms(seconds: f64) -> u64 { seconds_to_ms_f64(seconds) as u64 }
/// Convert kilograms to tonnes.
pub fn kg_to_tonnes(kg: f64) -> f64 { kg / KILOGRAMS_PER_TONNE }
/// Convert metres to kilometres.
pub fn metres_to_km(metres: f64) -> f64 { metres / METRES_PER_KILOMETRE }
/// Shared player and NPC targeting duration from effective dogma attributes.
pub fn lock_time_ms(scan_resolution: f64, signature_radius: f64) -> u64 {
    let signature = signature_radius.max(1.0).asinh();
    seconds_to_ms((LOCK_TIME_FACTOR_SECONDS / (scan_resolution.max(1.0) * signature * signature))
        .clamp(MIN_LOCK_TIME_SECONDS, MAX_LOCK_TIME_SECONDS))
}

/// Simulation clock in Unix milliseconds (not a Destiny event stamp).
pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as i64
}

/// Destiny event stamp in whole Unix seconds.
pub fn now_tick() -> i64 {
    destiny_stamp(now_ms())
}

#[derive(Clone)]
pub struct Warp {
    pub from: V3,
    pub to: V3,
    /// Unix milliseconds.
    pub start: i64,
    /// Milliseconds.
    pub duration: f64,
    pub factor: i64,
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
    /// Cached effective pilot-ship dogma signature radius for actor targeting; not serialized to destiny.
    pub signature_radius: Option<f64>,
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
    /// Last integration time in Unix milliseconds; missiles retain their launch time.
    pub last_ms: i64,
    /// Cloak mode (0 = visible). Cloaked balls exist only for their owner.
    pub cloak: u8,
    /// When an arrival cloak lapses (FILETIME), to tell a lapse from a later cloak.
    pub cloak_until: i64,
    /// Shield, armor and structure left, as fractions (damage state).
    pub health: [f64; DAMAGE_LAYER_COUNT],
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
                o.extend_from_slice(&(destiny_stamp(self.last_ms) as i32).to_le_bytes()); // effectStamp: launch tick
                for c in self.goto {
                    o.extend_from_slice(&c.to_le_bytes());
                }
            }
            MODE_WARP => {
                let w = self.warp.clone().unwrap_or(Warp { from: self.pos, to: self.goto, start: now_ms(), duration: 0.0, factor: DEFAULT_WARP_FACTOR });
                for c in w.to {
                    o.extend_from_slice(&c.to_le_bytes());
                }
                o.extend_from_slice(&(destiny_stamp(w.start) as i32).to_le_bytes()); // effectStamp
                o.extend_from_slice(&dist(w.to, w.from).to_le_bytes()); // total warp length
                // minRange (a double in mFollowId's slot); 0 because `to` already includes it.
                o.extend_from_slice(&0.0f64.to_le_bytes());
                o.extend_from_slice(&w.factor.to_le_bytes());
            }
            _ => {}
        }
    }

    /// `((shield, shieldRechargeTau, time), armor, structure)` (spaceObject.OnDamageState).
    pub fn damage_state(&self) -> Value {
        let [shield, armor, hull] = self.health;
        Value::tuple([
            Value::tuple([Value::Float(shield), Value::Float(DAMAGE_STATE_SHIELD_RECHARGE_TAU), Value::Int(crate::client::filetime_now())]),
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

    /// Exact integration for a constant target velocity over elapsed milliseconds.
    fn evolve(&mut self, ms: i64, others: &HashMap<i64, V3>) {
        // Missile launch time is also the stream's effectStamp.
        if self.mode == MODE_MISSILE { return; }
        if !self.free || ms <= self.last_ms {
            self.last_ms = self.last_ms.max(ms);
            return;
        }
        if let Some(w) = &self.warp {
            let elapsed = (ms - w.start) as f64;
            if elapsed >= w.duration {
                self.pos = w.to;
                self.vel = V3::default();
                self.warp = None;
                self.mode = MODE_STOP;
                self.speed_fraction = 0.0;
            } else {
                self.pos = add(w.from, scale(sub(w.to, w.from), (elapsed / w.duration.max(1.0)).clamp(0.0, 1.0)));
            }
            self.last_ms = ms;
            return;
        }
        let dt = ms_to_seconds((ms - self.last_ms) as f64);
        let tau = (self.mass * self.agility as f64 / MASS_AGILITY_SCALE).max(MIN_MOVEMENT_TAU_SECONDS);
        let speed = (self.max_vel * self.speed_fraction) as f64;
        let target = match self.mode {
            MODE_GOTO => scale(norm(self.goto), speed),
            MODE_FOLLOW | MODE_ORBIT => others.get(&self.follow_id).map(|&p| {
                let d = sub(p, self.pos);
                if len(d) <= self.follow_range as f64 + self.radius as f64 { V3::default() }
                else { scale(norm(d), speed) }
            }).unwrap_or_default(),
            _ => V3::default(),
        };
        let decay = (-dt / tau).exp();
        let delta = sub(self.vel, target);
        self.pos = add(self.pos, add(scale(target, dt), scale(delta, tau * -(-dt / tau).exp_m1())));
        self.vel = add(target, scale(delta, decay));
        self.last_ms = ms;
    }
}

/// Ballpark::SetupWarpConstants -> total warp duration in seconds.
pub fn warp_duration(factor: f64, distance: f64) -> f64 {
    let accel = factor * WARP_FACTOR_TO_ACCELERATION;
    let decel = (factor * WARP_FACTOR_TO_DECELERATION).min(MAX_WARP_DECELERATION);
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

/// `(stamp, (funcName, args))`
pub fn event(tick: i64, name: &str, args: Vec<Value>) -> Value {
    Value::tuple([Value::Int(tick), Value::tuple([Value::str(name), Value::Tuple(args)])])
}

/// Shared notification registry and admission queues, safe to use inside an actor.
#[derive(Default)]
pub struct Pilots {
    capacity: usize,
    notify: Mutex<HashMap<i64, NotifyTx>>,
    queues: Mutex<HashMap<i64, JumpQueue>>,
}
impl Pilots {
    pub fn register_pilot(&self, char_id: i64, tx: NotifyTx) {
        self.notify.lock().unwrap().insert(char_id, tx);
    }

    pub fn unregister_pilot(&self, char_id: i64) {
        self.notify.lock().unwrap().remove(&char_id);
        for q in self.queues.lock().unwrap().values_mut() {
            q.waiting.retain(|c| *c != char_id);
        }
    }

    pub fn notify_pilot(&self, char_id: i64, event: &str, args: Vec<Value>) {
        if let Some(tx) = self.notify.lock().unwrap().get(&char_id) {
            let _ = tx.send((event.to_string(), args));
        }
    }
    /// May `char_id` jump into `system_id` now? `Err(position)` puts them in its jump queue
    /// (jumpQueue.py); a key issued from the queue lets them in within JUMP_KEY_LIFE.
    pub fn request_entry(&self, system_id: i64, char_id: i64, population: usize) -> Result<(), i64> {
        if self.capacity == 0 {
            return Ok(());
        }
        {
            let mut queues = self.queues.lock().unwrap();
            let q = queues.entry(system_id).or_default();
            let now = crate::client::filetime_now();
            q.keys.retain(|_, issued| now - *issued < JUMP_KEY_LIFE * FILETIME_SECOND);
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
        self.pump_queue(system_id, population);
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
    pub fn pump_queue(&self, system_id: i64, population: usize) {
        if self.capacity == 0 {
            return;
        }
        let (update, recipients) = {
            let mut queues = self.queues.lock().unwrap();
            let Some(q) = queues.get_mut(&system_id) else { return };
            let now = crate::client::filetime_now();
            q.keys.retain(|_, issued| now - *issued < JUMP_KEY_LIFE * FILETIME_SECOND);
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
}

/// Actor facade. Actor closures must never await, access the DB or call World.
pub struct World {
    db: Db,
    systems: Arc<Mutex<HashMap<i64, SystemHandle>>>,
    loads: Mutex<HashMap<i64, Arc<tokio::sync::Mutex<()>>>>,
    observers: Mutex<HashMap<i64, i64>>,
    pilots: Arc<Pilots>,
}
impl World {
    pub fn new(db: Db, capacity: usize) -> Arc<Self> {
        Arc::new(Self { db, systems: Default::default(), loads: Default::default(),
            observers: Default::default(), pilots: Arc::new(Pilots { capacity, ..Pilots::default() }) })
    }
    pub fn register_pilot(&self, char_id: i64, tx: NotifyTx) { self.pilots.register_pilot(char_id, tx); }
    pub fn unregister_pilot(&self, char_id: i64) { self.pilots.unregister_pilot(char_id); }
    pub fn notify_pilot(&self, char_id: i64, name: &str, args: Vec<Value>) { self.pilots.notify_pilot(char_id, name, args); }
    /// Start the destination before removing a jumping ship from its source.
    /// Database loading is serialized per system, outside the actor.
    pub async fn system(&self, system_id: i64) -> Result<()> {
        if self.running(system_id).is_some() { return Ok(()); }
        let load = self.loads.lock().unwrap().entry(system_id).or_default().clone();
        let _guard = load.lock().await;
        if self.running(system_id).is_some() { return Ok(()); }
        let rows = self
            .db
            .query(
                "SELECT c.item_id, c.type_id, COALESCE(c.radius, t.radius, $2::float8)::float8 AS radius, c.x, c.y, c.z,
                        COALESCE(s.owner_id, 1)::bigint AS owner_id, c.group_id::bigint,
                        g.dest_stargate_id::bigint, g.dest_solar_system_id::bigint, gr.category_id
                 FROM sde.map_celestials c JOIN sde.inv_types t USING (type_id)
                 LEFT JOIN sde.inv_groups gr ON gr.group_id = c.group_id
                 LEFT JOIN sde.sta_stations s ON s.station_id = c.item_id
                 LEFT JOIN sde.map_stargates g ON g.stargate_id = c.item_id
                 WHERE c.solar_system_id = $1",
                &[&(system_id as i32), &CELESTIAL_FALLBACK_RADIUS],
            )
            .await?;
        let tick = now_ms();
        let mut system = System::new(system_id, self.pilots.clone());
        for r in rows {
            let id: i64 = r.get("item_id");
            let pos: V3 = [r.get("x"), r.get("y"), r.get("z")];
            let group = r.get::<_, Option<i64>>("group_id").unwrap_or(0) as i32;
            let category = r.get::<_, Option<i32>>("category_id").unwrap_or(0);
            if let Some(point) = warpin::warp_in_point(id, category, group, pos, r.get("radius")) {
                system.set_warp_in(id, point);
            }
            let mut slim = Vec::new();
            if let (Some(dest_gate), Some(dest_sys)) = (r.get::<_, Option<i64>>("dest_stargate_id"), r.get::<_, Option<i64>>("dest_solar_system_id")) {
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
            system.insert(
                Ball {
                    id,
                    type_id: r.get::<_, Option<i32>>("type_id").unwrap_or(0),
                    owner_id: r.get("owner_id"),
                    corp_id: r.get("owner_id"),
                    alliance_id: -1,
                    char_id: None,
                    signature_radius: None,
                    mode: MODE_RIGID,
                    radius: r.get::<_, f64>("radius") as f32,
                    mass: CELESTIAL_STATIC_MASS,
                    max_vel: 0.0,
                    agility: 1.0,
                    speed_fraction: 0.0,
                    pos,
                    vel: V3::default(),
                    goto: V3::default(),
                    follow_id: 0,
                    follow_range: 0.0,
                    free: false,
                    global: true,
                    massive: true,
                    warp: None,
                    warp_factor: 0,
                    last_ms: tick,
                    cloak: 0,
                    cloak_until: 0,
                    health: [1.0; 3],
                    launcher: 0,
                    slim,
                },
            );
        }
        info!("loaded solar system {system_id} with {} celestials", system.balls().count());

        let handle = actor::start(system, system_id, Arc::downgrade(&self.systems), self.db.clone());
        self.systems.lock().unwrap().insert(system_id, handle);
        Ok(())
    }
    fn running(&self, id: i64) -> Option<SystemHandle> {
        let mut systems = self.systems.lock().unwrap();
        if systems.get(&id).is_some_and(|h| h.is_closed()) { systems.remove(&id); }
        systems.get(&id).cloned()
    }
    /// Flush changed pilots in currently running systems without loading any systems.
    pub async fn flush_positions(&self) -> Result<u64> {
        let handles: Vec<_> = self.systems.lock().unwrap().values().filter(|h| !h.is_closed()).cloned().collect();
        let mut flushes = tokio::task::JoinSet::new();
        for handle in handles {
            flushes.spawn(snapshot::flush(handle, self.db.clone()));
        }
        let mut written = 0;
        let mut errors = Vec::new();
        while let Some(result) = flushes.join_next().await {
            match result {
                Ok(Ok(count)) => written += count,
                Ok(Err(error)) => errors.push(format!("{error:#}")),
                Err(error) => errors.push(format!("position flush task failed: {error}")),
            }
        }
        if errors.is_empty() { Ok(written) }
        else { Err(anyhow::anyhow!("position flush failed after writing {written} positions: {}", errors.join("; "))) }
    }
    /// No implicit loading. A missing/dead actor returns None.
    pub async fn with<R: Send + 'static>(&self, id: i64, f: impl FnOnce(&mut System) -> R + Send + 'static) -> Option<R> {
        self.running(id)?.with(f).await
    }
    /// No implicit loading; no-op if the system is not running.
    pub fn post(&self, id: i64, f: impl FnOnce(&mut System) + Send + 'static) {
        if let Some(handle) = self.running(id) { let _ = handle.send(Box::new(f)); }
    }
    /// No implicit loading; no-op if the system is not running. The delay starts
    /// when the actor processes this message; the closure runs on a due tick (D3).
    pub fn schedule(&self, system: i64, delay_ms: u64, f: impl FnOnce(&mut System) + Send + 'static) {
        self.post(system, move |s| s.schedule(delay_ms, f));
    }
    /// Wait for a timer tick without loading a missing system.
    pub async fn after(&self, system: i64, delay_ms: u64) -> bool {
        actor::after(self.running(system), delay_ms).await
    }
    pub async fn ball(&self, system: i64, id: i64) -> Option<Ball> {
        self.with(system, move |s| s.ball(id).cloned()).await.flatten()
    }
    pub async fn add_ship(&self, system: i64, ball: Ball) -> Result<()> {
        self.system(system).await?;
        self.with(system, move |s| s.add_ship(ball)).await.ok_or_else(|| anyhow::anyhow!("system actor stopped"))
    }
    pub async fn remove_ball(&self, system: i64, id: i64) -> Option<Ball> {
        self.with(system, move |s| s.remove_ball(id)).await.flatten()
    }
    pub async fn destroy_ball(&self, system: i64, id: i64) -> Option<Ball> {
        self.with(system, move |s| s.destroy_ball(id)).await.flatten()
    }
    pub async fn uncloak(&self, system: i64, id: i64) -> bool {
        self.with(system, move |s| s.uncloak(id)).await.unwrap_or(false)
    }
    pub async fn request_entry(&self, system: i64, pilot: i64) -> Result<(), i64> {
        self.with(system, move |s| s.request_entry(pilot)).await.unwrap_or(Err(1))
    }
    pub fn set_health(&self, system: i64, id: i64, health: [f64; DAMAGE_LAYER_COUNT]) {
        self.post(system, move |s| s.set_health(id, health));
    }
    pub fn broadcast(&self, system: i64, about: i64, events: Vec<Value>) {
        self.post(system, move |s| s.broadcast(about, events));
    }
    pub fn command(&self, system: i64, ship: i64, f: impl FnOnce(&mut Ball, i64, &System) -> Vec<Value> + Send + 'static) {
        self.post(system, move |s| s.command(ship, f));
    }
    pub async fn observe(&self, system: i64, client: i64, ego: i64, tx: DestinyTx) -> Result<Value> {
        self.system(system).await?;
        let previous = self.observers.lock().unwrap().insert(client, system);
        if let Some(previous) = previous.filter(|&p| p != system) {
            self.with(previous, move |s| s.unobserve(client)).await;
        }
        self.with(system, move |s| s.observe(client, ego, tx)).await.ok_or_else(|| anyhow::anyhow!("system actor stopped"))
    }
    pub fn unobserve(&self, client: i64) {
        let system = self.observers.lock().unwrap().remove(&client);
        if let Some(system) = system { self.post(system, move |s| s.unobserve(client)); }
    }
}

#[cfg(test)] mod tests;
