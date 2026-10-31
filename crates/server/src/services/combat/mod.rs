//! Targeting, ammunition, weapon groups and weapons fire.
//!
//! The client runs its own dogma for the active ship (clientDogmaLocation) and asks the
//! server through the bound dogmaIM: `AddTarget` (targetMgr._LockTarget), `LoadAmmo` /
//! `UnloadAmmo`, `LinkWeapons`..., and `Activate` / `Deactivate` (godma). The server
//! answers with notifications: `OnTarget`, `OnModuleAttributeChanges` (charge quantities
//! of the (shipID, flagID, typeID) charge sublocation), `OnGodmaShipEffect` (cycle
//! start/stop), `OnDamageMessage`, and destiny events for everyone in the system
//! (`OnSpecialFX`, `OnDamageStateChange`, missiles, `TerminalPlayDestructionEffect`).
//!
//! Weapons use their effective attributes (services::fit: skills, hull bonuses, damage
//! mods, charges): turret hit chance from optimal/falloff with a random hit quality,
//! missiles from flight range and explosion radius vs. signature, applied through shield,
//! armor and structure with their resonances. Every cycle draws capacitor (recharging on
//! the game's curve), reloading takes the module's reload time in space, and spawned NPCs
//! (spawn_npc) engage their mission runner and whoever shoots them: orbit, lock, shoot back
//! and repair themselves; a pilot's ship can be destroyed. Active modules apply their effects
//! (hardeners through fit, propulsion speed via queued destiny commands, repairers each cycle), and a kill
//! pays the NPC's bounty and leaves a wreck with loot.
//! NPC effects, charge dimensions and wreck mappings come from immutable SDE datasets;
//! loot prices come from `game.retail_prices` and static eligibility is applied before sampling.
//! Target locks, reloads, module cycles, pulses and missile impacts use system
//! actor timers. Database-backed effects run on deferred workers outside the actor.
//! Targeting and weapon range decisions use positions sampled together by the system actor;
//! wrecks and ejected pilots use the position sampled when their ball is destroyed.
//! Reload reservations survive interruptions and return to cargo during startup; reload
//! timers remain memory-only and are not resumed.
//! Dropped cycle owners release their own in-memory activation markers without notifications.
//! Reset clears a pilot's targets and pending locks and stops only that pilot's modules.
//! Missiles in flight use a temporary, nonpersistent ID namespace.
//! LoadAmmo accepts an owned active or assembled docked fitting ship and owned charge
//! stacks in its holds, the current hangar, or a directly held singleton container.
//! eve/client/script/dogma/clientDogmaLocation.py:980-1001,1072 forwards the actual
//! source location; eve/client/script/ui/shared/fittingScreen/tryFit.py:187-223 and
//! eve/client/script/ui/shared/fitting/fittingUtil.py:226-237 accept inventory sources.
//! eve/client/script/dogma/clientDogmaLocation.py:91-120 offers singleton charges
//! through `LoadChargesToModule` as well as stacked charges.
//! eve/common/script/util/inventoryFlagsCommon.py:9-15,53-60,217-226 describes cargo,
//! fleet and ammo holds. Remote, foreign and recursively nested containers are unsupported.

use crate::dao::items::loading::{Item, ItemQuery};
use super::items::load_items;
use super::inventory::{notify_item_change, notify_item_removed};
use crate::dao::items::quantities::{self, QuantityChange};
use super::fit::{Fit, Key};
use super::{db_err, notify_error, rand01, user_error, value_id_list, Call, CallError, CallResult};
use crate::client::{filetime_now, filetime_ticks_from_ms, Client};
use crate::consts::{ACCOUNT_CASH, ATTR_ARMOR_DAMAGE, ATTR_CAPACITOR_CAPACITY, ATTR_CAPACITY, ATTR_CHARGE, ATTR_EXPLOSION_DELAY, ATTR_HULL_DAMAGE, ATTR_MAX_RANGE, ATTR_MAX_VELOCITY, ATTR_QUANTITY, ATTR_SHIELD_CAPACITY, ATTR_SHIELD_CHARGE, ATTR_SIGNATURE_RADIUS, ATTR_STRUCTURE_HP, ATTR_FALLOFF, ATTR_AOE_CLOUD_SIZE, ATTR_MISSILE_DAMAGE_MULTIPLIER, CATEGORY_CHARGE, DAMAGE_TYPE_COUNT, FILETIME_SECOND, FLAG_CARGO, FLAG_HANGAR, IX_FLAG, IX_LOCATION_ID, IX_STACK_SIZE};
use crate::consts::{ATTR_DAMAGE_MULTIPLIER, ATTR_RATE_OF_FIRE, ATTR_EM_DAMAGE, ATTR_THERMAL_DAMAGE, ATTR_KINETIC_DAMAGE, ATTR_EXPLOSIVE_DAMAGE};
use crate::consts::{ATTR_ARMOR_HP, ATTR_SHIELD_EM_DAMAGE_RESONANCE, ATTR_SHIELD_THERMAL_DAMAGE_RESONANCE, ATTR_SHIELD_KINETIC_DAMAGE_RESONANCE, ATTR_SHIELD_EXPLOSIVE_DAMAGE_RESONANCE, ATTR_ARMOR_EM_DAMAGE_RESONANCE, ATTR_ARMOR_THERMAL_DAMAGE_RESONANCE, ATTR_ARMOR_KINETIC_DAMAGE_RESONANCE, ATTR_ARMOR_EXPLOSIVE_DAMAGE_RESONANCE, ATTR_STRUCTURE_EM_DAMAGE_RESONANCE, ATTR_STRUCTURE_THERMAL_DAMAGE_RESONANCE, ATTR_STRUCTURE_KINETIC_DAMAGE_RESONANCE, ATTR_STRUCTURE_EXPLOSIVE_DAMAGE_RESONANCE, ATTR_SCAN_RESOLUTION, ATTR_MAX_TARGET_RANGE, ATTR_MAX_LOCKED_TARGETS, ATTR_RECHARGE_RATE, ATTR_SHIELD_RECHARGE_RATE, ATTR_RELOAD_TIME, ATTR_NEXT_ACTIVATION_TIME, ATTR_ENTITY_FLY_RANGE, ATTR_ENTITY_ATTACK_RANGE, ATTR_ENTITY_MISSILE_TYPE, ATTR_ENTITY_KILL_BOUNTY, ATTR_ENTITY_SHIELD_BOOST_AMOUNT, ATTR_ENTITY_SHIELD_BOOST_DURATION, ATTR_ENTITY_ARMOR_REPAIR_AMOUNT, ATTR_ENTITY_ARMOR_REPAIR_DURATION};
use crate::db::Db;
use crate::db::executor::Executor;
use crate::db::outbox::{Outbox, dispatch_client, dispatch_world};
use crate::messages;
use crate::sde::Sde;
use crate::space::{dist, lock_time_ms, metres_to_km, ms_to_seconds, seconds_to_ms, now_ms, Ball, NotifyTx, NpcAction, NpcBrain, NpcConfig, NpcEffects, NpcFuture, World};
use marshal::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use tracing::{debug, info, warn};

const HI_SLOTS: std::ops::RangeInclusive<i32> = 27..=34;
/// inventorycommon/const.py:177, flagSpecializedAmmoHold.
const FLAG_AMMO_HOLD: i32 = 143;
/// inventorycommon/const.py:189, flagFleetHangar.
const FLAG_FLEET_HANGAR: i32 = 155;
pub(super) const ATTR_DAMAGE: [i32; 4] = [ATTR_EM_DAMAGE, ATTR_THERMAL_DAMAGE, ATTR_KINETIC_DAMAGE, ATTR_EXPLOSIVE_DAMAGE];
const ATTR_HP: [i32; 3] = [ATTR_SHIELD_CAPACITY, ATTR_ARMOR_HP, ATTR_STRUCTURE_HP];
const ATTR_RESONANCE: [[i32; 4]; 3] = [
    [ATTR_SHIELD_EM_DAMAGE_RESONANCE, ATTR_SHIELD_THERMAL_DAMAGE_RESONANCE, ATTR_SHIELD_KINETIC_DAMAGE_RESONANCE, ATTR_SHIELD_EXPLOSIVE_DAMAGE_RESONANCE],
    [ATTR_ARMOR_EM_DAMAGE_RESONANCE, ATTR_ARMOR_THERMAL_DAMAGE_RESONANCE, ATTR_ARMOR_KINETIC_DAMAGE_RESONANCE, ATTR_ARMOR_EXPLOSIVE_DAMAGE_RESONANCE],
    [ATTR_STRUCTURE_EM_DAMAGE_RESONANCE, ATTR_STRUCTURE_THERMAL_DAMAGE_RESONANCE, ATTR_STRUCTURE_KINETIC_DAMAGE_RESONANCE, ATTR_STRUCTURE_EXPLOSIVE_DAMAGE_RESONANCE],
];

/// Existing server reload fallback when dogma has no reload time.
const DEFAULT_RELOAD_MS: f64 = 10_000.0;
/// Server fallback missile velocity in metres per second when NPC charge dogma is missing it.
const NPC_MISSILE_VELOCITY_MPS: f64 = 3_000.0;
/// Server fallback missile flight time in milliseconds when NPC charge dogma is missing it.
const NPC_MISSILE_FLIGHT_MS: f64 = 5_000.0;
/// Server fallback explosion radius in metres when NPC charge dogma is missing it.
const NPC_MISSILE_EXPLOSION_RADIUS_METRES: f64 = 40.0;
/// Server policy for NPC turret reach beyond optimal, in multiples of falloff.
const NPC_TURRET_REACH_FALLOFF_MULTIPLIER: f64 = 3.0;
static NEXT_LOCK: AtomicU64 = AtomicU64::new(1);

// ---------------------------------------------------------------- state

#[derive(Clone)]
struct Hp {
    now: [f64; 3],
    max: [f64; 3],
    resonance: [[f64; 4]; 3],
}

pub(super) struct ActiveModule {
    pub(super) char_id: i64,
    pub(super) stop: Arc<AtomicBool>,
}

#[derive(Default)]
pub(super) struct State {
    /// charID -> locked targets
    targets: HashMap<i64, HashSet<i64>>,
    /// (charID, targetID) -> lock attempt in progress
    locking: HashMap<(i64, i64), u64>,
    /// moduleID -> owner and stop flag of its running cycle
    pub(super) active: HashMap<i64, ActiveModule>,
    /// Most recent activation, retained to invalidate deferred propulsion updates.
    pub(super) activation_generation: HashMap<i64, Arc<AtomicBool>>,
    /// ballID -> hit points left (NPCs)
    hp: HashMap<i64, Hp>,
    /// shipID -> (capacitor level, FILETIME it was measured)
    capacitor: HashMap<i64, (f64, i64)>,
    /// shipID -> a player ship's damage
    condition: HashMap<i64, Condition>,
    /// moduleID -> FILETIME its reload finishes
    pub(super) reloading: HashMap<i64, i64>,
}

impl State {
    fn reset_character(&mut self, char_id: i64) {
        self.targets.remove(&char_id);
        self.locking.retain(|(ch, _), _| *ch != char_id);
        for active in self.active.values().filter(|active| active.char_id == char_id) {
            active.stop.store(true, Ordering::Relaxed);
        }
    }
}

/// Modules currently running their activation effect (services::fit applies them).
pub fn active_modules() -> HashSet<i64> {
    state().active.keys().copied().collect()
}

pub(super) fn state() -> MutexGuard<'static, State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(Default::default).lock().unwrap()
}

/// What a cycle task needs once the call that started it has returned.
#[derive(Clone)]
pub(super) struct Ctx {
    pub(super) db: Db,
    pub(super) world: Arc<World>,
    pub(super) notify: NotifyTx,
    pub(super) char_id: i64,
    pub(super) ship_id: i64,
    pub(super) system_id: i64,
}

impl Ctx {
    pub(super) fn notify(&self, event: &str, args: Vec<Value>) {
        let _ = self.notify.send((event.to_string(), args));
    }
}

pub(super) fn ctx(c: &Client) -> Option<Ctx> {
    Some(Ctx {
        db: c.db.clone(),
        world: c.world.clone(),
        notify: c.notify_tx.clone(),
        char_id: c.session.int("charid")?,
        ship_id: c.session.int("shipid")?,
        system_id: c.session.int("solarsystemid")?,
    })
}


pub(super) fn attr(sde: &Sde, type_id: i32, id: i32, default: f64) -> f64 {
    sde.types.attribute(type_id, id).unwrap_or(default)
}

/// Stop everything a character was doing in space (docking, jumping, logging off).
pub fn reset(c: &Client) {
    let Some(char_id) = c.session.int("charid") else { return };
    state().reset_character(char_id);
}


mod reload_recovery;
pub(crate) use reload_recovery::recover_pending_reloads;
pub(super) mod targeting_charges;
use targeting_charges::*;
pub(crate) mod groups_health;
use groups_health::*;
use super::effects::notify::attribute_change;
use super::effects::lifecycle;
pub(crate) mod npc;
use npc::*;

pub(super) fn quantity_change(owner: i64, ship: i64, flag: i32, type_id: i32, new: i64, old: i64) -> Value {
    Value::list([attribute_change(owner, charge_key(ship, flag, type_id), ATTR_QUANTITY, Value::Int(new), Value::Int(old))])
}

pub async fn flag_quantities(c: &Client, ship: i64) -> Result<(Value, Vec<Value>), CallError> {
    let rows = crate::dao::items::charges::all(&c.db, ship).await.map_err(db_err)?;
    let header = marshal::DbRowDescriptor::new([
        ("locationID", marshal::DbType::I8),
        ("flagID", marshal::DbType::I2),
        ("typeID", marshal::DbType::I4),
        ("quantity", marshal::DbType::I4),
    ]);
    let mut by_flag = Vec::new();
    let mut keys = Vec::new();
    for r in &rows {
        let (flag, type_id, qty) = (r.flag_id, r.type_id, r.quantity);
        by_flag.push((
            Value::Int(flag as i64),
            header.row(&[Value::Int(ship), Value::Int(flag as i64), Value::Int(type_id as i64), Value::Int(qty as i64)]),
        ));
        keys.push(charge_key(ship, flag, type_id));
    }
    let cache = if by_flag.is_empty() { Value::Dict(vec![]) } else { Value::Dict(vec![(Value::Int(ship), Value::Dict(by_flag))]) };
    Ok((cache, keys))
}

pub async fn banks(db: &(impl Executor + ?Sized), ship: i64) -> Result<Value, CallError> {
    let rows = crate::dao::items::charges::banks(db, ship).await.map_err(db_err)?;
    let mut by_master: Vec<(i64, Vec<Value>)> = Vec::new();
    for r in rows {
        let (master, slave) = (r.master_id, r.slave_id);
        match by_master.iter_mut().find(|(m, _)| *m == master) {
            Some((_, v)) => v.push(Value::Int(slave)),
            None => by_master.push((master, vec![Value::Int(slave)])),
        }
    }
    Ok(Value::Dict(by_master.into_iter().map(|(m, s)| (Value::Int(m), Value::List(s))).collect()))
}

pub fn ship_condition(ship: i64) -> Option<(f64, f64, f64)> {
    let st = state();
    let c = st.condition.get(&ship)?;
    let shield = recharge(c.now[0], c.stats.max[0], c.stats.shield_tau_ms, (filetime_now() - c.shield_at) as f64 / FILETIME_SECOND as f64);
    Some((shield, c.stats.max[1] - c.now[1], c.stats.max[2] - c.now[2]))
}

pub fn repair(ship: i64) {
    let mut st = state();
    st.condition.remove(&ship);
    st.capacitor.remove(&ship);
}

pub fn refill_capacitor(ship: i64) {
    state().capacitor.remove(&ship);
}

pub fn forget_ship(ship: i64) {
    let mut st = state();
    st.condition.remove(&ship);
    st.capacitor.remove(&ship);
}

pub(super) async fn destroy_player_ship(world: &World, system: i64, ship: i64, char_id: i64) {
    state().condition.remove(&ship);
    if let Some(b) = world.destroy_ball(system, ship).await {
        forget_target(world, ship);
        world.notify_pilot(char_id, "__ship_destroyed", vec![Value::Int(ship), Value::Float(b.pos[0]), Value::Float(b.pos[1]), Value::Float(b.pos[2])]);
    }
}


pub async fn spawn_npc(db: Db, world: Arc<World>, system: i64, id: i64, type_id: i32, owner: Option<i64>) -> Result<(), CallError> {
    let stats = match npc_stats(&db, world.sde(), type_id).await {
        Ok(stats) => Arc::new(stats),
        Err(error) => {
            world.post(system, move |s| { s.remove_ball(id); });
            return Err(error);
        }
    };
    let config = NpcConfig {
        scan_resolution: stats.scan_resolution,
        fly_range: stats.fly_range,
        aggro_range: stats.aggro_range,
        rate_of_fire_ms: stats.rate_of_fire_ms,
        repairs: stats.repairs,
    };
    let weak_world = Arc::downgrade(&world);
    let effects: NpcEffects = Arc::new(move |action| -> NpcFuture {
        let db = db.clone();
        let weak_world = weak_world.clone();
        let stats = stats.clone();
        Box::pin(async move {
            let Some(world) = weak_world.upgrade() else { return };
            match action {
                NpcAction::Shot { me, victim } => {
                    if let Err(e) = npc_shot(&db, &world, system, id, type_id, &stats, me, victim).await {
                        warn!("npc {id}: {}", match e {
                            CallError::Internal(e) => format!("{e:#}"),
                            CallError::User { msg, .. } => msg,
                        });
                    }
                }
                NpcAction::Repair => npc_repair(&world, system, id, &stats),
            }
        })
    });
    let brain = NpcBrain::new(owner, config, effects);
    world.post(system, move |s| {
        s.add_npc(id, brain);
    });
    Ok(())
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    Some(match call.method {
        "AddTarget" => add_target(c, call.args.first().and_then(|v| v.as_int()).unwrap_or(0)).await,
        "CancelAddTarget" => {
            if let (Some(ch), Some(t)) = (c.session.int("charid"), call.args.first().and_then(|v| v.as_int())) {
                state().locking.remove(&(ch, t));
            }
            Ok(Value::None)
        }
        "RemoveTarget" => {
            let t = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
            drop_targets(c, &[t]);
            Ok(Value::None)
        }
        "RemoveTargets" => {
            drop_targets(c, &value_id_list(call.args.first()));
            Ok(Value::None)
        }
        "ClearTargets" => {
            if let Some(ch) = c.session.int("charid") {
                state().targets.remove(&ch);
            }
            c.notify("OnTarget", vec![Value::str("clear")]);
            Ok(Value::None)
        }
        "GetTargets" => Ok(Value::list(c.session.int("charid").map(targets_of).unwrap_or_default().into_iter().map(Value::Int))),
        "GetTargeters" => Ok(Value::list([])),
        "LoadAmmo" => load_ammo(c, call).await,
        "UnloadAmmo" => unload_ammo(c, call).await,
        "LinkWeapons" | "LinkAllWeapons" | "UnlinkModule" | "UnlinkAllModules" | "MergeModuleGroups" | "PeelAndLink" | "GetWeaponBankInfoForShip" => {
            weapon_groups(c, call).await
        }
        "Activate" => lifecycle::activate(c, call).await,
        "Deactivate" => lifecycle::deactivate(c, call).await,
        _ => return None,
    })
}

#[cfg(test)] mod tests;

