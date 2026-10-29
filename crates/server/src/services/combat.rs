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
//! (hardeners through fit, propulsion speed via destiny, repairers each cycle), and a kill
//! pays the NPC's bounty and leaves a wreck with loot.
//! NPC decisions and missile impact timers run in the per-system actor, with weapon,
//! repair and missile damage effects resolved by its worker outside the actor.

use super::dogma::load_items;
use super::inventory::notify_item_change;
use super::fit::{Fit, Key};
use super::{db_err, rand01, user_error, Call, CallError, CallResult};
use crate::client::{filetime_now, Client};
use crate::consts::{ACCOUNT_CASH, ATTR_CAPACITY, ATTR_EXPLOSION_DELAY, ATTR_IS_ONLINE, ATTR_MASS, ATTR_MAX_RANGE, ATTR_MAX_VELOCITY, ATTR_QUANTITY, ATTR_SIGNATURE_RADIUS, EFFECT_AFTERBURNER, EFFECT_MICROWARPDRIVE, EFFECT_ONLINE, FILETIME_SECOND, FLAG_CARGO, FLAG_HANGAR, IX_FLAG, IX_LOCATION_ID, IX_STACK_SIZE, LOCATION_JUNKYARD};
use crate::db::Db;
use crate::space::{dist, event, now_ms, now_tick, Ball, NotifyTx, NpcAction, NpcBrain, NpcConfig, NpcEffects, NpcFuture, World, MODE_MISSILE, MODE_WARP, V3};
use marshal::Value;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use tracing::{debug, info, warn};

const HI_SLOTS: std::ops::RangeInclusive<i32> = 27..=34;
const ATTR_DAMAGE_MULTIPLIER: i32 = 64;
const ATTR_FALLOFF: i32 = 158;
const ATTR_DAMAGE: [i32; 4] = [114, 118, 117, 116]; // em, thermal, kinetic, explosive
const ATTR_HP: [i32; 3] = [263, 265, 9]; // shield, armor, structure
const ATTR_RESONANCE: [[i32; 4]; 3] = [[271, 274, 273, 272], [267, 270, 269, 268], [113, 110, 109, 111]];
const ATTR_SCAN_RESOLUTION: i32 = 564;
const ATTR_MAX_TARGET_RANGE: i32 = 76;
const ATTR_MAX_LOCKED_TARGETS: i32 = 192;
const ATTR_AOE_CLOUD_SIZE: i32 = 654;
const ATTR_CHARGE: i32 = 18; // capacitor level
const ATTR_CAPACITOR_CAPACITY: i32 = 482;
const ATTR_RECHARGE_RATE: i32 = 55;
const ATTR_CAPACITOR_NEED: i32 = 6;
const ATTR_SHIELD_CHARGE: i32 = 264;
const ATTR_ARMOR_DAMAGE: i32 = 266;
const ATTR_HULL_DAMAGE: i32 = 3;
const ATTR_SHIELD_RECHARGE_RATE: i32 = 479;
const ATTR_RELOAD_TIME: i32 = 1795;
const ATTR_NEXT_ACTIVATION_TIME: i32 = 2729;
const ATTR_MISSILE_DAMAGE_MULTIPLIER: i32 = 212;
const ATTR_RATE_OF_FIRE: i32 = 51;
const ATTR_ENTITY_FLY_RANGE: i32 = 416;
const ATTR_ENTITY_ATTACK_RANGE: i32 = 247;
const ATTR_ENTITY_MISSILE_TYPE: i32 = 507;
const ATTR_ENTITY_KILL_BOUNTY: i32 = 481;
const ATTR_ENTITY_SHIELD_BOOST_AMOUNT: i32 = 637;
const ATTR_ENTITY_SHIELD_BOOST_DURATION: i32 = 636;
const ATTR_ENTITY_ARMOR_REPAIR_AMOUNT: i32 = 631;
const ATTR_ENTITY_ARMOR_REPAIR_DURATION: i32 = 630;
const ATTR_SHIELD_BONUS: i32 = 68;
const ATTR_ARMOR_DAMAGE_AMOUNT: i32 = 84;
const ATTR_STRUCTURE_DAMAGE_AMOUNT: i32 = 83;
const UE_TYPEID: i64 = 4;

/// Temporary item IDs for missiles in flight.
/// Missile balls; clear of NPCs (9.0e12), exploration sites (9.5e12), probes (9.6e12) and
/// asteroids (9.7e12).
static NEXT_MISSILE_ID: AtomicI64 = AtomicI64::new(9_800_000_000_000);
static NEXT_LOCK: AtomicU64 = AtomicU64::new(1);

// ---------------------------------------------------------------- state

#[derive(Clone)]
struct Hp {
    now: [f64; 3],
    max: [f64; 3],
    resonance: [[f64; 4]; 3],
}

#[derive(Default)]
struct State {
    /// charID -> locked targets
    targets: HashMap<i64, HashSet<i64>>,
    /// (charID, targetID) -> lock attempt in progress
    locking: HashMap<(i64, i64), u64>,
    /// moduleID -> stop flag of its running cycle
    active: HashMap<i64, Arc<AtomicBool>>,
    /// ballID -> hit points left (NPCs)
    hp: HashMap<i64, Hp>,
    /// shipID -> (capacitor level, FILETIME it was measured)
    capacitor: HashMap<i64, (f64, i64)>,
    /// shipID -> a player ship's damage
    condition: HashMap<i64, Condition>,
    /// moduleID -> FILETIME its reload finishes
    reloading: HashMap<i64, i64>,
}

/// Modules currently running their activation effect (services::fit applies them).
pub fn active_modules() -> HashSet<i64> {
    state().active.keys().copied().collect()
}

fn state() -> MutexGuard<'static, State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(Default::default).lock().unwrap()
}

/// What a cycle task needs once the call that started it has returned.
#[derive(Clone)]
struct Ctx {
    db: Db,
    world: Arc<World>,
    notify: NotifyTx,
    char_id: i64,
    ship_id: i64,
    system_id: i64,
}

impl Ctx {
    fn notify(&self, event: &str, args: Vec<Value>) {
        let _ = self.notify.send((event.to_string(), args));
    }
}

fn ctx(c: &Client) -> Option<Ctx> {
    Some(Ctx {
        db: c.db.clone(),
        world: c.world.clone(),
        notify: c.notify_tx.clone(),
        char_id: c.session.int("charid")?,
        ship_id: c.session.int("shipid")?,
        system_id: c.session.int("solarsystemid")?,
    })
}

async fn attributes(db: &Db, type_id: i32) -> Result<HashMap<i32, f64>, CallError> {
    let rows = db.client.query("SELECT attribute_id, value FROM sde.type_attributes WHERE type_id = $1", &[&type_id]).await.map_err(db_err)?;
    Ok(rows.iter().map(|r| (r.get(0), r.get(1))).collect())
}

fn attr(a: &HashMap<i32, f64>, id: i32, default: f64) -> f64 {
    a.get(&id).copied().unwrap_or(default)
}

/// Stop everything a character was doing in space (docking, jumping, logging off).
pub fn reset(c: &Client) {
    let Some(char_id) = c.session.int("charid") else { return };
    let mut st = state();
    st.targets.remove(&char_id);
    st.locking.retain(|(ch, _), _| *ch != char_id);
    // Cycle tasks check their ship is still in space; flag them all to stop.
    for stop in st.active.values() {
        stop.store(true, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------- targeting

fn targets_of(char_id: i64) -> Vec<i64> {
    state().targets.get(&char_id).map(|t| t.iter().copied().collect()).unwrap_or_default()
}

/// AddTarget(targetID) -> (lockPending, targets). The lock completes after the lock
/// time from the ship's scan resolution and the target's signature radius, with
/// `OnTarget('add', targetID)`.
async fn add_target(c: &mut Client, target: i64) -> CallResult {
    let Some(cx) = ctx(c) else { return Err(user_error("TargetingAttemptCancelled", vec![])) };
    let ship_id = cx.ship_id;
    let Some((ship, t)) = cx.world.with(cx.system_id, move |s| Some((s.ball(ship_id)?.clone(), s.ball(target)?.clone()))).await.flatten() else {
        return Err(user_error("TargetingAttemptCancelled", vec![]));
    };
    let fit = Fit::load(&cx.db, cx.char_id, cx.ship_id).await?;
    let target_attrs = attributes(&cx.db, t.type_id).await?;
    let range = fit.value(Key::Ship, ATTR_MAX_TARGET_RANGE);
    let gap = dist(ship.pos, t.pos) - t.radius as f64;
    if gap > range {
        let text = format!("目标距离 {:.1} 公里，超出了你的锁定距离（{:.1} 公里）。", gap / 1000.0, range / 1000.0);
        return Err(user_error("CustomNotify", vec![("notify".into(), Value::unicode(text))]));
    }
    // The ship's limit, capped by the pilot's (Targeting skills).
    let pilot_limit = fit.value(Key::Char, ATTR_MAX_LOCKED_TARGETS);
    let ship_limit = fit.value(Key::Ship, ATTR_MAX_LOCKED_TARGETS);
    let max_targets = if pilot_limit > 0.0 { ship_limit.min(pilot_limit) } else { ship_limit } as usize;
    if targets_of(cx.char_id).len() >= max_targets.max(1) {
        return Err(user_error("CustomNotify", vec![("notify".into(), Value::unicode(format!("你最多只能同时锁定 {} 个目标。", max_targets.max(1))))]));
    }
    // Lock time (s) = 40000 / (scanResolution * asinh(signatureRadius)^2)
    let scan = fit.value(Key::Ship, ATTR_SCAN_RESOLUTION).max(1.0);
    let sig = attr(&target_attrs, ATTR_SIGNATURE_RADIUS, t.radius as f64).max(1.0);
    let seconds = (40_000.0 / (scan * sig.asinh().powi(2))).clamp(0.5, 60.0);
    let attempt = NEXT_LOCK.fetch_add(1, Ordering::Relaxed);
    state().locking.insert((cx.char_id, target), attempt);
    debug!(user = c.login.user_id, "locking {target} in {seconds:.1}s");
    let current = targets_of(cx.char_id);
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs_f64(seconds)).await;
        let still_wanted = {
            let mut st = state();
            st.locking.get(&(cx.char_id, target)) == Some(&attempt) && st.locking.remove(&(cx.char_id, target)).is_some()
        };
        let present = cx.world.with(cx.system_id, move |s| s.contains(target)).await.unwrap_or(false);
        if still_wanted && present {
            state().targets.entry(cx.char_id).or_default().insert(target);
            cx.notify("OnTarget", vec![Value::str("add"), Value::Int(target)]);
        } else if still_wanted {
            cx.notify("OnTarget", vec![Value::str("lost"), Value::Int(target), Value::None]);
        }
    });
    Ok(Value::tuple([Value::Bool(true), Value::list(current.into_iter().map(Value::Int))]))
}

fn drop_targets(c: &mut Client, targets: &[i64]) {
    let Some(char_id) = c.session.int("charid") else { return };
    {
        let mut st = state();
        if let Some(set) = st.targets.get_mut(&char_id) {
            for t in targets {
                set.remove(t);
            }
        }
    }
    for t in targets {
        c.notify("OnTarget", vec![Value::str("lost"), Value::Int(*t), Value::None]);
    }
}

/// A destroyed ball is no longer anyone's target.
fn forget_target(world: &World, target: i64) {
    let holders: Vec<i64> = {
        let mut st = state();
        st.hp.remove(&target);
        st.locking.retain(|(_, t), _| *t != target);
        st.targets.iter_mut().filter_map(|(ch, set)| set.remove(&target).then_some(*ch)).collect()
    };
    for ch in holders {
        world.notify_pilot(ch, "OnTarget", vec![Value::str("lost"), Value::Int(target), Value::str("Exploding")]);
    }
}

// ---------------------------------------------------------------- charges

/// The charge sublocation key `(shipID, flagID, typeID)`.
fn charge_key(ship: i64, flag: i32, type_id: i32) -> Value {
    Value::tuple([Value::Int(ship), Value::Int(flag as i64), Value::Int(type_id as i64)])
}

/// `OnModuleAttributeChanges` for a charge sublocation's quantity (clientDogmaLocation
/// fits/unfits the sublocation from it).
pub(super) fn quantity_change(owner: i64, ship: i64, flag: i32, type_id: i32, new: i64, old: i64) -> Value {
    Value::list([Value::tuple([
        Value::str("OnModuleAttributeChange"),
        Value::Int(owner),
        charge_key(ship, flag, type_id),
        Value::Int(ATTR_QUANTITY as i64),
        Value::Int(filetime_now()),
        Value::Int(new),
        Value::Int(old),
        Value::None,
    ])])
}

/// `{shipID: {flagID: row(locationID, flagID, typeID, quantity)}}` for the ship state.
pub async fn flag_quantities(c: &Client, ship: i64) -> Result<(Value, Vec<Value>), CallError> {
    let rows = c
        .db
        .client
        .query("SELECT flag_id, type_id, quantity FROM game.module_charges WHERE ship_id = $1", &[&ship])
        .await
        .map_err(db_err)?;
    let header = marshal::DbRowDescriptor::new([
        ("locationID", marshal::DbType::I8),
        ("flagID", marshal::DbType::I2),
        ("typeID", marshal::DbType::I4),
        ("quantity", marshal::DbType::I4),
    ]);
    let mut by_flag = Vec::new();
    let mut keys = Vec::new();
    for r in &rows {
        let (flag, type_id, qty): (i32, i32, i32) = (r.get(0), r.get(1), r.get(2));
        by_flag.push((
            Value::Int(flag as i64),
            header.row(&[Value::Int(ship), Value::Int(flag as i64), Value::Int(type_id as i64), Value::Int(qty as i64)]),
        ));
        keys.push(charge_key(ship, flag, type_id));
    }
    let cache = if by_flag.is_empty() { Value::Dict(vec![]) } else { Value::Dict(vec![(Value::Int(ship), Value::Dict(by_flag))]) };
    Ok((cache, keys))
}

async fn loaded(db: &Db, ship: i64, flag: i32) -> Result<Option<(i32, i32)>, CallError> {
    Ok(db
        .client
        .query_opt("SELECT type_id, quantity FROM game.module_charges WHERE ship_id = $1 AND flag_id = $2", &[&ship, &flag])
        .await
        .map_err(db_err)?
        .map(|r| (r.get(0), r.get(1))))
}

/// Tell the client a stack is gone, then delete it.
pub(super) async fn remove_stack(c: &mut Client, item: super::dogma::Item) -> Result<(), CallError> {
    c.db.client.execute("DELETE FROM game.items WHERE item_id = $1", &[&item.item_id]).await.map_err(db_err)?;
    let old = item.location_id;
    let gone = super::dogma::Item { location_id: LOCATION_JUNKYARD, ..item };
    c.notify("OnItemChange", vec![gone.row(), Value::Dict(vec![(Value::Int(IX_LOCATION_ID), Value::Int(old))]), Value::None]);
    Ok(())
}

/// Put `quantity` x `type_id` into `location`/`flag`, onto an existing stack if there is one.
pub(super) async fn deposit(c: &mut Client, owner: i64, location: i64, flag: i32, type_id: i32, quantity: i32, from: i64) -> Result<(), CallError> {
    let stack = c
        .db
        .client
        .query_opt(
            "SELECT item_id, quantity FROM game.items WHERE location_id = $1 AND flag_id = $2 AND type_id = $3 AND owner_id = $4 AND NOT singleton LIMIT 1",
            &[&location, &flag, &type_id, &owner],
        )
        .await
        .map_err(db_err)?;
    match stack {
        Some(r) => {
            let (id, old): (i64, i32) = (r.get(0), r.get(1));
            c.db.client.execute("UPDATE game.items SET quantity = quantity + $2 WHERE item_id = $1", &[&id, &quantity]).await.map_err(db_err)?;
            notify_item_change(c, id, vec![(IX_STACK_SIZE, Value::Int(old as i64))]).await
        }
        None => {
            let id: i64 = c
                .db
                .client
                .query_one(
                    "INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity) VALUES ($1, $2, $3, $4, $5) RETURNING item_id",
                    &[&type_id, &owner, &location, &flag, &quantity],
                )
                .await
                .map_err(db_err)?
                .get(0);
            notify_item_change(c, id, vec![(IX_LOCATION_ID, Value::Int(from)), (IX_FLAG, Value::Int(flag as i64))]).await
        }
    }
}

/// Modules of a weapon group, master first.
async fn bank(db: &Db, ship: i64, master: i64) -> Result<Vec<i64>, CallError> {
    let rows = db.client.query("SELECT slave_id FROM game.weapon_banks WHERE ship_id = $1 AND master_id = $2", &[&ship, &master]).await.map_err(db_err)?;
    Ok(std::iter::once(master).chain(rows.iter().map(|r| r.get(0))).collect())
}

/// Empty a module's charges into `location`/`flag` (cargo by default).
async fn unload(c: &mut Client, ship: i64, module: &super::dogma::Item, location: i64, flag: i32) -> Result<(), CallError> {
    let Some((type_id, qty)) = loaded(&c.db, ship, module.flag_id).await? else { return Ok(()) };
    c.db.client.execute("DELETE FROM game.module_charges WHERE ship_id = $1 AND flag_id = $2", &[&ship, &module.flag_id]).await.map_err(db_err)?;
    let owner = c.session.int("charid").unwrap_or(module.owner_id);
    c.notify("OnModuleAttributeChanges", vec![quantity_change(owner, ship, module.flag_id, type_id, 0, qty as i64)]);
    deposit(c, owner, location, flag, type_id, qty, ship).await
}

/// LoadAmmo(shipID, moduleID | [moduleIDs], [chargeItemIDs], chargeLocationID): fill each
/// module (and its group) with charges from the given stacks, swapping out other types.
/// In space the charges go in after the module's reload time (OnChargeBeingLoadedToModule
/// and nextActivationTime drive the client's reload animation); docked it is immediate.
async fn load_ammo(c: &mut Client, call: &Call<'_>) -> CallResult {
    let ship = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
    let mut modules: Vec<i64> = match call.args.get(1) {
        Some(v) if v.items().is_some() => v.items().unwrap().iter().filter_map(|m| m.as_int()).collect(),
        Some(v) => v.as_int().into_iter().collect(),
        None => vec![],
    };
    let charge_ids: Vec<i64> = call.args.get(2).and_then(|v| v.items()).unwrap_or_default().iter().filter_map(|v| v.as_int()).collect();
    let mut all = vec![];
    for m in modules.drain(..) {
        for id in bank(&c.db, ship, m).await? {
            if !all.contains(&id) {
                all.push(id);
            }
        }
    }
    let owner = c.session.int("charid").unwrap_or(0);
    // (module, flag, charge type, quantity after, quantity before)
    let mut loads: Vec<(i64, i32, i32, i32, i32)> = vec![];
    for module_id in all {
        let Some(module) = load_items(c, "i.item_id = $1 AND i.location_id = $2", &[&module_id, &ship]).await?.into_iter().next() else { continue };
        let stacks = load_items(c, "i.item_id = ANY($1) AND i.quantity > 0", &[&charge_ids]).await?;
        let Some(first) = stacks.first() else { break };
        let type_id = first.type_id;
        // Capacity is the type's own column (the dogma attribute only exists on some types).
        let capacity: f64 = c
            .db
            .client
            .query_one(
                "SELECT GREATEST(COALESCE(t.capacity, 0), COALESCE((SELECT value FROM sde.type_attributes WHERE type_id = t.type_id AND attribute_id = $2), 0))::float8
                 FROM sde.inv_types t WHERE t.type_id = $1",
                &[&module.type_id, &ATTR_CAPACITY],
            )
            .await
            .map_err(db_err)?
            .get(0);
        let volume: f64 = c
            .db
            .client
            .query_one("SELECT COALESCE(volume, 0.01)::float8 FROM sde.inv_types WHERE type_id = $1", &[&type_id])
            .await
            .map_err(db_err)?
            .get(0);
        let max = (capacity / volume.max(1e-6) + 1e-6).floor() as i32;
        if max <= 0 {
            continue;
        }
        let current = loaded(&c.db, ship, module.flag_id).await?;
        let have = match current {
            Some((t, q)) if t == type_id => q,
            Some(_) => {
                unload(c, ship, &module, ship, FLAG_CARGO).await?;
                0
            }
            None => 0,
        };
        let mut need = max - have;
        for stack in stacks.into_iter().filter(|s| s.type_id == type_id) {
            if need <= 0 {
                break;
            }
            let take = need.min(stack.quantity);
            need -= take;
            if take == stack.quantity {
                remove_stack(c, stack).await?;
            } else {
                c.db.client.execute("UPDATE game.items SET quantity = quantity - $2 WHERE item_id = $1", &[&stack.item_id, &take]).await.map_err(db_err)?;
                notify_item_change(c, stack.item_id, vec![(IX_STACK_SIZE, Value::Int(stack.quantity as i64))]).await?;
            }
        }
        let now = max - need;
        if now > have {
            loads.push((module_id, module.flag_id, type_id, now, have));
        }
    }
    let Some(&(first, _, type_id, _, _)) = loads.first() else { return Ok(Value::None) };
    let (db, notify) = (c.db.clone(), c.notify_tx.clone());
    if c.session.int("solarsystemid").is_none() {
        apply_loads(&db, &notify, owner, ship, &loads).await?;
        return Ok(Value::None);
    }
    let reload_ms = match Fit::load(&c.db, owner, ship).await?.value(Key::Module(first), ATTR_RELOAD_TIME) {
        ms if ms > 0.0 => ms,
        _ => 10_000.0,
    };
    let start = filetime_now();
    let until = start + (reload_ms * 10_000.0) as i64;
    let ids: Vec<i64> = loads.iter().map(|l| l.0).collect();
    {
        let mut st = state();
        for id in &ids {
            st.reloading.insert(*id, until);
        }
    }
    // (moduleIDs, chargeTypeID, reloadTime ms): the HUD's reload ramp and "done in N s" message.
    c.notify("OnChargeBeingLoadedToModule", vec![Value::list(ids.iter().map(|i| Value::Int(*i))), Value::Int(type_id as i64), Value::Int(reload_ms as i64)]);
    let timers = ids.iter().map(|i| attribute_change(owner, Value::Int(*i), ATTR_NEXT_ACTIVATION_TIME, Value::Int(until), Value::Int(start))).collect();
    c.notify("OnModuleAttributeChanges", vec![Value::List(timers)]);
    info!(user = c.login.user_id, "reloading {} module(s) with {type_id} in {reload_ms:.0} ms", ids.len());
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs_f64(reload_ms / 1000.0)).await;
        if let Err(e) = apply_loads(&db, &notify, owner, ship, &loads).await {
            warn!("reload of {ids:?} failed: {}", match e {
                CallError::Internal(e) => format!("{e:#}"),
                CallError::User { msg, .. } => msg,
            });
        }
        let mut st = state();
        for id in &ids {
            if st.reloading.get(id) == Some(&until) {
                st.reloading.remove(id);
            }
        }
    });
    Ok(Value::None)
}

/// Put loaded charges in their modules and tell the client.
async fn apply_loads(db: &Db, notify: &NotifyTx, owner: i64, ship: i64, loads: &[(i64, i32, i32, i32, i32)]) -> Result<(), CallError> {
    for &(module, flag, type_id, now, have) in loads {
        db.client
            .execute(
                "INSERT INTO game.module_charges (ship_id, flag_id, type_id, quantity) VALUES ($1, $2, $3, $4)
                 ON CONFLICT (ship_id, flag_id) DO UPDATE SET type_id = EXCLUDED.type_id, quantity = EXCLUDED.quantity",
                &[&ship, &flag, &type_id, &now],
            )
            .await
            .map_err(db_err)?;
        let _ = notify.send(("OnModuleAttributeChanges".into(), vec![quantity_change(owner, ship, flag, type_id, now as i64, have as i64)]));
        debug!("loaded {now} x {type_id} into module {module}");
    }
    Ok(())
}

/// UnloadAmmo(shipID, moduleID | [moduleIDs], destination[, quantity]); destination is a
/// container ID or (containerID, ownerID, flag).
async fn unload_ammo(c: &mut Client, call: &Call<'_>) -> CallResult {
    let ship = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
    let modules: Vec<i64> = match call.args.get(1) {
        Some(v) if v.items().is_some() => v.items().unwrap().iter().filter_map(|m| m.as_int()).collect(),
        Some(v) => v.as_int().into_iter().collect(),
        None => vec![],
    };
    let (location, flag) = match call.args.get(2) {
        Some(v) if v.items().is_some() => {
            let t = v.items().unwrap();
            (t.first().and_then(|x| x.as_int()).unwrap_or(ship), t.get(2).and_then(|x| x.as_int()).map(|f| f as i32))
        }
        Some(v) => (v.as_int().unwrap_or(ship), None),
        None => (ship, None),
    };
    let flag = flag.unwrap_or(if location == ship { FLAG_CARGO } else { FLAG_HANGAR });
    for m in modules {
        for id in bank(&c.db, ship, m).await? {
            if let Some(module) = load_items(c, "i.item_id = $1 AND i.location_id = $2", &[&id, &ship]).await?.into_iter().next() {
                unload(c, ship, &module, location, flag).await?;
            }
        }
    }
    Ok(Value::None)
}

// ---------------------------------------------------------------- weapon groups

/// `{masterID: [slaveIDs]}` for a ship.
pub async fn banks(db: &Db, ship: i64) -> Result<Value, CallError> {
    let rows = db.client.query("SELECT master_id, slave_id FROM game.weapon_banks WHERE ship_id = $1 ORDER BY slave_id", &[&ship]).await.map_err(db_err)?;
    let mut by_master: Vec<(i64, Vec<Value>)> = Vec::new();
    for r in rows {
        let (master, slave): (i64, i64) = (r.get(0), r.get(1));
        match by_master.iter_mut().find(|(m, _)| *m == master) {
            Some((_, v)) => v.push(Value::Int(slave)),
            None => by_master.push((master, vec![Value::Int(slave)])),
        }
    }
    Ok(Value::Dict(by_master.into_iter().map(|(m, s)| (Value::Int(m), Value::List(s))).collect()))
}

async fn link(db: &Db, ship: i64, master: i64, slave: i64) -> Result<(), CallError> {
    if master == slave {
        return Ok(());
    }
    // A master joining another group brings its own slaves along.
    db.client.execute("UPDATE game.weapon_banks SET master_id = $2 WHERE ship_id = $1 AND master_id = $3", &[&ship, &master, &slave]).await.map_err(db_err)?;
    db.client
        .execute(
            "INSERT INTO game.weapon_banks (ship_id, master_id, slave_id) VALUES ($1, $2, $3) ON CONFLICT (slave_id) DO UPDATE SET master_id = EXCLUDED.master_id",
            &[&ship, &master, &slave],
        )
        .await
        .map_err(db_err)?;
    Ok(())
}

async fn weapon_groups(c: &mut Client, call: &Call<'_>) -> CallResult {
    let int = |i: usize| call.args.get(i).and_then(|v| v.as_int()).unwrap_or(0);
    let ship = int(0);
    let db = c.db.clone();
    match call.method {
        "LinkWeapons" | "MergeModuleGroups" | "PeelAndLink" => {
            let (master, other) = (int(1), int(2));
            if call.method == "PeelAndLink" {
                db.client.execute("DELETE FROM game.weapon_banks WHERE ship_id = $1 AND slave_id = $2", &[&ship, &other]).await.map_err(db_err)?;
                db.client
                    .execute("INSERT INTO game.weapon_banks (ship_id, master_id, slave_id) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING", &[&ship, &master, &other])
                    .await
                    .map_err(db_err)?;
            } else {
                link(&db, ship, master, other).await?;
            }
            banks(&db, ship).await
        }
        // Group every online high-slot module with the others of its type.
        "LinkAllWeapons" => {
            db.client.execute("DELETE FROM game.weapon_banks WHERE ship_id = $1", &[&ship]).await.map_err(db_err)?;
            let rows = db
                .client
                .query(
                    "SELECT item_id, type_id FROM game.items WHERE location_id = $1 AND flag_id BETWEEN $2 AND $3 AND online ORDER BY flag_id",
                    &[&ship, &HI_SLOTS.start(), &HI_SLOTS.end()],
                )
                .await
                .map_err(db_err)?;
            let mut masters: HashMap<i32, i64> = HashMap::new();
            for r in rows {
                let (id, type_id): (i64, i32) = (r.get(0), r.get(1));
                match masters.get(&type_id) {
                    Some(&m) => link(&db, ship, m, id).await?,
                    None => {
                        masters.insert(type_id, id);
                    }
                }
            }
            banks(&db, ship).await
        }
        // Takes the last module off a group; returns it.
        "UnlinkModule" => {
            let master = int(1);
            let slave: Option<i64> = db
                .client
                .query_opt(
                    "DELETE FROM game.weapon_banks WHERE slave_id = (SELECT max(slave_id) FROM game.weapon_banks WHERE ship_id = $1 AND master_id = $2) RETURNING slave_id",
                    &[&ship, &master],
                )
                .await
                .map_err(db_err)?
                .map(|r| r.get(0));
            Ok(slave.map(Value::Int).unwrap_or(Value::None))
        }
        "UnlinkAllModules" => {
            db.client.execute("DELETE FROM game.weapon_banks WHERE ship_id = $1", &[&ship]).await.map_err(db_err)?;
            Ok(Value::None)
        }
        _ => banks(&db, if ship != 0 { ship } else { c.session.int("shipid").unwrap_or(0) }).await,
    }
}

// ---------------------------------------------------------------- capacitor and hit points

/// Capacitor or shield after `dt` seconds of passive recharge from `level` (the game's
/// curve: recharge peaks at 25%, ~98% after one recharge time `tau_ms`).
fn recharge(level: f64, max: f64, tau_ms: f64, dt: f64) -> f64 {
    if max <= 0.0 {
        return 0.0;
    }
    let tau = (tau_ms / 1000.0).max(0.001);
    let c = 1.0 + ((level / max).clamp(0.0, 1.0).sqrt() - 1.0) * (-5.0 * dt / tau).exp();
    (max * c * c).min(max)
}

/// One `OnModuleAttributeChanges` entry.
fn attribute_change(owner: i64, item: Value, attribute: i32, new: Value, old: Value) -> Value {
    Value::tuple([
        Value::str("OnModuleAttributeChange"),
        Value::Int(owner),
        item,
        Value::Int(attribute as i64),
        Value::Int(filetime_now()),
        new,
        old,
        Value::None,
    ])
}

/// Use `amount` of the ship's capacitor; false (and nothing used) when there isn't enough.
fn drain_capacitor(cx: &Ctx, fit: &Fit, amount: f64) -> bool {
    if amount <= 0.0 {
        return true;
    }
    let (max, tau) = (fit.value(Key::Ship, ATTR_CAPACITOR_CAPACITY), fit.value(Key::Ship, ATTR_RECHARGE_RATE));
    let now = filetime_now();
    let current = {
        let mut st = state();
        let (level, at) = st.capacitor.get(&cx.ship_id).copied().unwrap_or((max, now));
        let current = recharge(level, max, tau, (now - at) as f64 / FILETIME_SECOND as f64);
        if current < amount {
            return false;
        }
        st.capacitor.insert(cx.ship_id, (current - amount, now));
        current
    };
    let change = attribute_change(cx.char_id, Value::Int(cx.ship_id), ATTR_CHARGE, Value::Float(current - amount), Value::Float(current));
    cx.notify("OnModuleAttributeChanges", vec![Value::list([change])]);
    true
}

/// A ship's hit points and resistances, from its fit.
#[derive(Clone)]
struct ShipStats {
    max: [f64; 3],
    resonance: [[f64; 4]; 3],
    shield_tau_ms: f64,
}

/// A player ship's condition: shield (recharging since `shield_at`), armor and structure left.
#[derive(Clone)]
struct Condition {
    stats: ShipStats,
    now: [f64; 3],
    shield_at: i64,
}

async fn ship_stats(db: &Db, char_id: i64, ship: i64) -> Result<ShipStats, CallError> {
    let fit = Fit::load(db, char_id, ship).await?;
    Ok(ShipStats {
        max: ATTR_HP.map(|a| fit.value(Key::Ship, a)),
        resonance: ATTR_RESONANCE.map(|layer| layer.map(|a| fit.value(Key::Ship, a))),
        shield_tau_ms: fit.value(Key::Ship, ATTR_SHIELD_RECHARGE_RATE),
    })
}

/// (shieldCharge, armorDamage, damage) of a ship that has taken damage since it was last
/// repaired, with its shield recharged to now; None for an undamaged ship.
pub fn ship_condition(ship: i64) -> Option<(f64, f64, f64)> {
    let st = state();
    let c = st.condition.get(&ship)?;
    let shield = recharge(c.now[0], c.stats.max[0], c.stats.shield_tau_ms, (filetime_now() - c.shield_at) as f64 / FILETIME_SECOND as f64);
    Some((shield, c.stats.max[1] - c.now[1], c.stats.max[2] - c.now[2]))
}

/// Docking repairs the ship.
pub fn repair(ship: i64) {
    let mut st = state();
    st.condition.remove(&ship);
    st.capacitor.remove(&ship);
}

/// A ship being (re)loaded by its pilot (undocking, boarding) starts with a full capacitor.
pub fn refill_capacitor(ship: i64) {
    state().capacitor.remove(&ship);
}

/// Forget a destroyed ship.
pub fn forget_ship(ship: i64) {
    let mut st = state();
    st.condition.remove(&ship);
    st.capacitor.remove(&ship);
}

/// Take raw damage (em, thermal, kinetic, explosive) through layers with resonances;
/// returns the damage dealt.
fn absorb(now: &mut [f64; 3], resonance: &[[f64; 4]; 3], raw: [f64; 4]) -> f64 {
    let mut left = raw;
    let mut dealt = 0.0;
    for layer in 0..3 {
        let effective: f64 = (0..4).map(|i| left[i] * resonance[layer][i]).sum();
        if effective <= 0.0 {
            break;
        }
        if effective <= now[layer] {
            now[layer] -= effective;
            dealt += effective;
            break;
        }
        // This layer breaks; what it didn't absorb carries on to the next.
        let used = now[layer] / effective;
        dealt += now[layer];
        now[layer] = 0.0;
        left = left.map(|d| d * (1.0 - used));
    }
    dealt
}

/// Damage an NPC (or any non-player ball); destroys it at zero structure.
async fn damage_npc(db: &Db, world: &World, system: i64, target: i64, raw: [f64; 4], attacker: Option<i64>) -> Result<f64, CallError> {
    let Some(ball) = world.ball(system, target).await else { return Ok(0.0) };
    if let Some(pilot) = attacker {
        world.post(system, move |s| {
            s.add_aggressor(target, pilot);
        });
    }
    let known = state().hp.get(&target).cloned();
    let mut hp = match known {
        Some(hp) => hp,
        None => {
            let a = attributes(db, ball.type_id).await?;
            let max = ATTR_HP.map(|id| attr(&a, id, 0.0));
            Hp { now: max, max, resonance: ATTR_RESONANCE.map(|layer| layer.map(|id| attr(&a, id, 1.0))) }
        }
    };
    let dealt = absorb(&mut hp.now, &hp.resonance, raw);
    if hp.now[2] <= 0.0 && (hp.max[2] > 0.0 || hp.now.iter().all(|v| *v <= 0.0)) {
        info!("ball {target} (type {}) destroyed", ball.type_id);
        world.destroy_ball(system, target).await;
        forget_target(world, target);
        if let Err(e) = reward_kill(db, world, system, &ball, attacker).await {
            warn!("wreck of {target}: {}", match e {
                CallError::Internal(e) => format!("{e:#}"),
                CallError::User { msg, .. } => msg,
            });
        }
    } else {
        let health = [0, 1, 2].map(|i| if hp.max[i] > 0.0 { (hp.now[i] / hp.max[i]).clamp(0.0, 1.0) } else { 0.0 });
        state().hp.insert(target, hp);
        world.set_health(system, target, health);
    }
    Ok(dealt)
}

/// Damage a player's ship; at zero structure it explodes and its pilot is told to eject
/// (`__ship_destroyed`, handled by the pilot's connection).
async fn damage_player(db: &Db, world: &World, system: i64, ship: i64, char_id: i64, raw: [f64; 4], attacker: Option<i64>) -> Result<f64, CallError> {
    // Resistances and hit points as fitted right now (hardeners running, modules offline...).
    let stats = ship_stats(db, char_id, ship).await?;
    let known = state().condition.get(&ship).cloned();
    let mut cond = match known {
        Some(mut c) => {
            c.now = [0, 1, 2].map(|i| c.now[i].min(stats.max[i]));
            c.stats = stats;
            c
        }
        None => Condition { now: stats.max, stats, shield_at: filetime_now() },
    };
    let now = filetime_now();
    let old = cond.now;
    cond.now[0] = recharge(cond.now[0], cond.stats.max[0], cond.stats.shield_tau_ms, (now - cond.shield_at) as f64 / FILETIME_SECOND as f64);
    cond.shield_at = now;
    let dealt = absorb(&mut cond.now, &cond.stats.resonance, raw);
    if let Some(attacker) = attacker {
        super::crimewatch::note_damage(ship, attacker, dealt);
    }
    let max = cond.stats.max;
    let changes = vec![
        attribute_change(char_id, Value::Int(ship), ATTR_SHIELD_CHARGE, Value::Float(cond.now[0]), Value::Float(old[0])),
        attribute_change(char_id, Value::Int(ship), ATTR_ARMOR_DAMAGE, Value::Float(max[1] - cond.now[1]), Value::Float(max[1] - old[1])),
        attribute_change(char_id, Value::Int(ship), ATTR_HULL_DAMAGE, Value::Float(max[2] - cond.now[2]), Value::Float(max[2] - old[2])),
    ];
    world.notify_pilot(char_id, "OnModuleAttributeChanges", vec![Value::List(changes)]);
    if cond.now[2] <= 0.0 {
        info!("ship {ship} of {char_id} destroyed");
        destroy_player_ship(world, system, ship, char_id).await;
    } else {
        let health = [0, 1, 2].map(|i| if max[i] > 0.0 { (cond.now[i] / max[i]).clamp(0.0, 1.0) } else { 0.0 });
        state().condition.insert(ship, cond);
        world.set_health(system, ship, health);
    }
    Ok(dealt)
}

/// A player's ship explodes: its pilot is told to eject (`__ship_destroyed`, handled by
/// the pilot's connection). Also how CONCORD ends a criminal.
pub(super) async fn destroy_player_ship(world: &World, system: i64, ship: i64, char_id: i64) {
    state().condition.remove(&ship);
    if let Some(b) = world.destroy_ball(system, ship).await {
        forget_target(world, ship);
        world.notify_pilot(char_id, "__ship_destroyed", vec![Value::Int(ship), Value::Float(b.pos[0]), Value::Float(b.pos[1]), Value::Float(b.pos[2])]);
    }
}

/// Damage whatever `target` is: a player's ship or an NPC. `attacker` is the pilot
/// responsible, if any (NPCs remember them; kills pay them).
async fn damage(db: &Db, world: &World, system: i64, target: i64, target_pilot: Option<i64>, raw: [f64; 4], attacker: Option<i64>) -> Result<f64, CallError> {
    match target_pilot {
        Some(char_id) => damage_player(db, world, system, target, char_id, raw, attacker).await,
        None => damage_npc(db, world, system, target, raw, attacker).await,
    }
}

/// Repair a player's ship layer (0 shield, 1 armor, 2 structure) by `amount`.
async fn repair_player(db: &Db, world: &World, system: i64, ship: i64, char_id: i64, layer: usize, amount: f64) -> Result<(), CallError> {
    let known = state().condition.get(&ship).cloned();
    // An undamaged ship has nothing to repair.
    let Some(mut cond) = known else { return Ok(()) };
    let now = filetime_now();
    let old = cond.now;
    cond.now[0] = recharge(cond.now[0], cond.stats.max[0], cond.stats.shield_tau_ms, (now - cond.shield_at) as f64 / FILETIME_SECOND as f64);
    cond.shield_at = now;
    cond.now[layer] = (cond.now[layer] + amount).min(cond.stats.max[layer]);
    let max = cond.stats.max;
    let changes = vec![
        attribute_change(char_id, Value::Int(ship), ATTR_SHIELD_CHARGE, Value::Float(cond.now[0]), Value::Float(old[0])),
        attribute_change(char_id, Value::Int(ship), ATTR_ARMOR_DAMAGE, Value::Float(max[1] - cond.now[1]), Value::Float(max[1] - old[1])),
        attribute_change(char_id, Value::Int(ship), ATTR_HULL_DAMAGE, Value::Float(max[2] - cond.now[2]), Value::Float(max[2] - old[2])),
    ];
    world.notify_pilot(char_id, "OnModuleAttributeChanges", vec![Value::List(changes)]);
    let health = [0, 1, 2].map(|i| if max[i] > 0.0 { (cond.now[i] / max[i]).clamp(0.0, 1.0) } else { 0.0 });
    state().condition.insert(ship, cond);
    world.set_health(system, ship, health);
    let _ = db;
    Ok(())
}

/// A destroyed NPC pays its bounty to the pilot who killed it and leaves a wreck (its
/// group's wreck type, sde_native.wrecks) holding loot worth about its bounty.
async fn reward_kill(db: &Db, world: &World, system: i64, npc: &Ball, killer: Option<i64>) -> Result<(), CallError> {
    let bounty = attr(&attributes(db, npc.type_id).await?, ATTR_ENTITY_KILL_BOUNTY, 0.0);
    if let Some(pilot) = killer.filter(|_| bounty > 0.0) {
        // A player corporation takes its tax rate of the bounty into its master wallet.
        let corp_tax = db
            .client
            .query_opt(
                "SELECT co.corporation_id, co.tax_rate FROM game.characters ch JOIN game.corporations co ON co.corporation_id = ch.corporation_id
                 WHERE ch.character_id = $1 AND NOT co.deleted",
                &[&pilot],
            )
            .await
            .map_err(db_err)?
            .map(|r| (r.get::<_, i64>(0), r.get::<_, f64>(1).clamp(0.0, 1.0)));
        let tax = corp_tax.map(|(_, rate)| (bounty * rate * 100.0).round() / 100.0).unwrap_or(0.0);
        let balance: f64 = db
            .client
            .query_one(
                "UPDATE game.characters SET balance = balance + $2::float8::numeric WHERE character_id = $1 RETURNING balance::float8",
                &[&pilot, &(bounty - tax)],
            )
            .await
            .map_err(db_err)?
            .get(0);
        world.notify_pilot(pilot, "OnAccountChange", vec![Value::str("cash"), Value::Int(pilot), Value::Float(balance)]);
        if let Some((corp, _)) = corp_tax.filter(|_| tax > 0.0) {
            super::corpwallet::corp_credit(db, world, corp, ACCOUNT_CASH, tax).await?;
        }
        info!("{pilot} earned {bounty:.0} ISK bounty for {} ({tax:.0} corporation tax)", npc.type_id);
        super::crimewatch::npc_killed(db, world, pilot).await?;
    }
    let wreck = db
        .client
        .query_opt(
            "SELECT COALESCE(w.data->'raceIdToWreckTypeId'->>(COALESCE(t.race_id, 0)::text), w.data->'raceIdToWreckTypeId'->>'0')::int,
                    COALESCE(wt.radius, 100)::float8
             FROM sde.inv_types t JOIN sde_native.wrecks w ON w.key = t.group_id::text
             LEFT JOIN sde.inv_types wt ON wt.type_id = COALESCE(w.data->'raceIdToWreckTypeId'->>(COALESCE(t.race_id, 0)::text), w.data->'raceIdToWreckTypeId'->>'0')::int
             WHERE t.type_id = $1",
            &[&npc.type_id],
        )
        .await
        .map_err(db_err)?;
    let Some(wreck) = wreck else { return Ok(()) };
    let Some(wreck_type) = wreck.get::<_, Option<i32>>(0) else { return Ok(()) };
    let owner = killer.unwrap_or(npc.owner_id);
    let wreck_id: i64 = db
        .client
        .query_one(
            "INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton, x, y, z)
             VALUES ($1, $2, $3, 0, 1, true, $4, $5, $6) RETURNING item_id",
            &[&wreck_type, &owner, &system, &npc.pos[0], &npc.pos[1], &npc.pos[2]],
        )
        .await
        .map_err(db_err)?
        .get(0);
    // Loot: one or two modules or ammo stacks that traded on Serenity, worth up to the bounty.
    let picks = 1 + (rand01() * 2.0) as i64;
    let loot = db
        .client
        .query(
            "SELECT t.type_id, t.category_id FROM sde.inv_types t JOIN game.retail_prices r USING (type_id)
             WHERE t.published AND t.category_id IN (7, 8) AND r.average BETWEEN 1000 AND GREATEST($1::float8, 50000)
             ORDER BY random() LIMIT $2",
            &[&bounty, &picks],
        )
        .await
        .map_err(db_err)?;
    for r in &loot {
        let (type_id, category): (i32, i32) = (r.get(0), r.get(1));
        let quantity: i32 = if category == 8 { 20 + (rand01() * 100.0) as i32 } else { 1 };
        db.client
            .execute(
                "INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity) VALUES ($1, $2, $3, 0, $4)",
                &[&type_id, &owner, &wreck_id, &quantity],
            )
            .await
            .map_err(db_err)?;
    }
    let ball = Ball {
        id: wreck_id,
        type_id: wreck_type,
        owner_id: owner,
        corp_id: npc.corp_id,
        alliance_id: -1,
        char_id: None,
        mode: crate::space::MODE_RIGID,
        radius: wreck.get::<_, f64>(1) as f32,
        mass: 10_000.0,
        max_vel: 0.0,
        agility: 1.0,
        speed_fraction: 0.0,
        pos: npc.pos,
        vel: [0.0; 3],
        goto: [0.0; 3],
        follow_id: 0,
        follow_range: 0.0,
        free: false,
        global: false,
        massive: true,
        warp: None,
        warp_factor: 0,
        last_ms: now_ms(),
        cloak: 0,
        cloak_until: 0,
        health: [1.0; 3],
        launcher: 0,
        slim: vec![("isEmpty", Value::Bool(loot.is_empty()))],
    };
    world.add_ship(system, ball).await.map_err(CallError::Internal)?;
    debug!("wreck {wreck_id} ({wreck_type}) with {} loot stacks", loot.len());
    Ok(())
}

// ---------------------------------------------------------------- weapons fire

#[derive(Clone, Copy, PartialEq)]
enum Weapon {
    Turret,
    Missile,
}

fn weapon_kind(effect: &str) -> Option<Weapon> {
    match effect {
        "targetAttack" | "projectileFired" | "targetDisintegratorAttack" => Some(Weapon::Turret),
        "useMissiles" => Some(Weapon::Missile),
        _ => None,
    }
}

#[derive(Clone)]
struct Effect {
    id: i64,
    guid: Option<String>,
    duration_attribute: Option<i32>,
    weapon: Option<Weapon>,
    /// Repairers: the layer they repair and the module attribute with the amount.
    repair: Option<(usize, i32)>,
    /// Afterburner / microwarpdrive: the ship's speed changes while it runs.
    propulsion: bool,
    /// Mining lasers and harvesters: the beam's effect guid; each cycle ends with a yield
    /// (services/mining).
    mining: Option<&'static str>,
}

fn repair_kind(effect: &str) -> Option<(usize, i32)> {
    match effect {
        "shieldBoosting" => Some((0, ATTR_SHIELD_BONUS)),
        "armorRepair" => Some((1, ATTR_ARMOR_DAMAGE_AMOUNT)),
        "structureRepair" => Some((2, ATTR_STRUCTURE_DAMAGE_AMOUNT)),
        _ => None,
    }
}

/// The ship's speed and mass as fitted now (propulsion running or not), for everyone's destiny.
async fn apply_propulsion(cx: &Ctx) {
    let Ok(fit) = Fit::load(&cx.db, cx.char_id, cx.ship_id).await else { return };
    let (speed, mass) = (fit.value(Key::Ship, ATTR_MAX_VELOCITY), fit.value(Key::Ship, ATTR_MASS));
    cx.world.command(cx.system_id, cx.ship_id, move |b, t, _| {
        b.max_vel = speed as f32;
        b.mass = mass;
        vec![
            event(t, "SetMaxSpeed", vec![Value::Int(b.id), Value::Float(speed)]),
            event(t, "SetBallMass", vec![Value::Int(b.id), Value::Float(mass)]),
        ]
    });
}

#[derive(Clone)]
struct Module {
    id: i64,
    type_id: i32,
    flag: i32,
}

/// `[self, char, ship, target, other, area, effectID, line]` (godma ENV_IDX_*).
fn environment(cx: &Ctx, module: i64, target: Option<i64>, effect: i64) -> Value {
    Value::list([
        Value::Int(module),
        Value::Int(cx.char_id),
        Value::Int(cx.ship_id),
        target.map(Value::Int).unwrap_or(Value::None),
        Value::None,
        Value::None,
        Value::Int(effect),
        Value::None,
    ])
}

/// OnGodmaShipEffect(itemID, effectID, time, start, active, environment, startTime,
/// duration, repeat, error, actualStopTime)
#[allow(clippy::too_many_arguments)]
fn ship_effect(cx: &Ctx, module: i64, effect: &Effect, target: Option<i64>, start: bool, started: i64, duration_ms: f64, repeat: i64) {
    let now = filetime_now();
    cx.notify(
        "OnGodmaShipEffect",
        vec![
            Value::Int(module),
            Value::Int(effect.id),
            Value::Int(now),
            Value::Int(start as i64),
            Value::Int(start as i64),
            environment(cx, module, target, effect.id),
            Value::Int(started),
            Value::Float(duration_ms),
            Value::Int(repeat),
            Value::None,
            if start { Value::None } else { Value::Int(now) },
        ],
    );
}

/// A weapon firing animation (fxSequencer.OnSpecialFX) from `shooter`.
#[allow(clippy::too_many_arguments)]
fn fire_fx(shooter: i64, module: Option<(i64, i32)>, guid: &str, target: Option<i64>, charge: Option<i32>, start: bool, duration_ms: f64, repeat: i64) -> Value {
    event(
        now_tick(),
        "OnSpecialFX",
        vec![
            Value::Int(shooter),
            module.map(|m| Value::Int(m.0)).unwrap_or(Value::None),
            module.map(|m| Value::Int(m.1 as i64)).unwrap_or(Value::None),
            target.map(Value::Int).unwrap_or(Value::None),
            charge.map(|t| Value::Int(t as i64)).unwrap_or(Value::None),
            Value::str(guid),
            Value::Bool(true),
            Value::Int(start as i64),
            Value::Int(start as i64),
            Value::Float(duration_ms),
            Value::Int(repeat),
        ],
    )
}

fn weapon_fx(cx: &Ctx, modules: &[Module], effect: &Effect, target: Option<i64>, charge: Option<i32>, start: bool, duration_ms: f64, repeat: i64) {
    let Some(guid) = effect.guid.as_deref().and_then(|g| g.split(',').next()) else { return };
    let events = modules
        .iter()
        .map(|m| fire_fx(cx.ship_id, Some((m.id, m.type_id)), guid, target, charge, start, duration_ms, repeat))
        .collect();
    cx.world.broadcast(cx.system_id, cx.ship_id, events);
}

/// Use one charge from a module; `None` when it is empty.
async fn consume_charge(cx: &Ctx, module: &Module) -> Result<Option<i32>, CallError> {
    let row = cx
        .db
        .client
        .query_opt(
            "UPDATE game.module_charges SET quantity = quantity - 1 WHERE ship_id = $1 AND flag_id = $2 AND quantity > 0 RETURNING type_id, quantity",
            &[&cx.ship_id, &module.flag],
        )
        .await
        .map_err(db_err)?;
    let Some(r) = row else { return Ok(None) };
    let (type_id, left): (i32, i32) = (r.get(0), r.get(1));
    if left == 0 {
        cx.db.client.execute("DELETE FROM game.module_charges WHERE ship_id = $1 AND flag_id = $2", &[&cx.ship_id, &module.flag]).await.map_err(db_err)?;
    }
    cx.notify("OnModuleAttributeChanges", vec![quantity_change(cx.char_id, cx.ship_id, module.flag, type_id, left as i64, left as i64 + 1)]);
    Ok(Some(type_id))
}

/// The game's hit quality for the combat log: 0 miss .. 6 wrecking.
fn hit_quality(multiplier: f64) -> i64 {
    match multiplier {
        m if m <= 0.0 => 0,
        m if m >= 2.9 => 6,
        m if m < 0.6 => 1,
        m if m < 0.8 => 2,
        m if m < 1.0 => 3,
        m if m < 1.2 => 4,
        _ => 5,
    }
}

/// A turret's damage multiplier for one shot: miss (0), 0.49..1.49, or a 1% wrecking 3x,
/// with the hit chance falling off past optimal range.
fn turret_roll(gap: f64, optimal: f64, falloff: f64) -> f64 {
    let chance = 0.5f64.powf(((gap - optimal).max(0.0) / falloff.max(1.0)).powi(2));
    let roll = rand01();
    if roll >= chance {
        0.0
    } else if roll < 0.01 {
        3.0
    } else {
        roll + 0.49
    }
}

/// `OnDamageMessage` for the shooter (`target`) or the victim (`source`).
fn damage_message(key: &str, other: i64, weapon: i32, damage: f64, quality: i64) -> Value {
    Value::dict([
        (key, Value::Int(other)),
        ("weapon", Value::Int(weapon as i64)),
        ("damage", Value::Float(damage)),
        ("hitQuality", Value::Int(quality)),
        ("isBanked", Value::Bool(false)),
        // uilog: "me" picks AttackHits ("to {target}"), "otherPlayerWeapons" AttackHitsR
        // ("from {source}").
        ("attackType", Value::str(if key == "source" { "otherPlayerWeapons" } else { "me" })),
        ("damageTypes", Value::Dict(vec![])),
    ])
}

/// Tell both sides of a hit: the shooter (if a pilot) and the victim (if a pilot).
fn report_hit(world: &World, shooter: i64, target: i64, shooter_pilot: Option<i64>, target_pilot: Option<i64>, weapon: i32, damage: f64, quality: i64) {
    if let Some(ch) = shooter_pilot {
        world.notify_pilot(ch, "OnDamageMessage", vec![damage_message("target", target, weapon, damage, quality)]);
    }
    if let Some(ch) = target_pilot {
        world.notify_pilot(ch, "OnDamageMessage", vec![damage_message("source", shooter, weapon, damage, quality)]);
    }
}

/// One turret shot at `target`, with the fitted module's and its charge's effective attributes.
async fn fire_turret(cx: &Ctx, fit: &Fit, module: &Module, target: i64) -> Result<(), CallError> {
    let ship_id = cx.ship_id;
    let Some((ship, t)) = cx.world.with(cx.system_id, move |s| Some((s.ball(ship_id)?.clone(), s.ball(target)?.clone()))).await.flatten() else { return Ok(()) };
    let (m, ch) = (Key::Module(module.id), Key::Charge(module.id));
    let gap = (dist(ship.pos, t.pos) - t.radius as f64 - ship.radius as f64).max(0.0);
    let multiplier = turret_roll(gap, fit.value(m, ATTR_MAX_RANGE), fit.value(m, ATTR_FALLOFF));
    let base = fit.value(m, ATTR_DAMAGE_MULTIPLIER).max(0.0) * multiplier;
    let raw = ATTR_DAMAGE.map(|id| fit.value(ch, id) * base);
    let dealt = if multiplier > 0.0 { damage(&cx.db, &cx.world, cx.system_id, target, t.char_id, raw, Some(cx.char_id)).await? } else { 0.0 };
    report_hit(&cx.world, cx.ship_id, target, ship.char_id, t.char_id, module.type_id, dealt, hit_quality(multiplier));
    Ok(())
}

/// What a missile does when it arrives.
struct Missile {
    charge: i32,
    raw: [f64; 4],
    aoe_cloud: f64,
    velocity: f64,
    flight_range: f64,
    weapon: i32,
    /// The pilot who fired it.
    attacker: Option<i64>,
}

/// Launch a missile ball from `from` at `target`: it flies client-side; on impact it
/// explodes and deals damage scaled by explosion radius vs. the target's signature.
async fn launch_missile(db: Db, world: Arc<World>, system: i64, from: Ball, target: Ball, owner: i64, module: Option<i64>, m: Missile) -> Result<(), CallError> {
    let size = db
        .client
        .query_one("SELECT COALESCE(radius, 1)::float8, COALESCE(mass, 1)::float8 FROM sde.inv_types WHERE type_id = $1", &[&m.charge])
        .await
        .map_err(db_err)?;
    let mut slim = vec![("sourceShipID", Value::Int(from.id))];
    slim.push(("launchModules", Value::list(module.map(Value::Int))));
    let missile = Ball {
        id: NEXT_MISSILE_ID.fetch_add(1, Ordering::Relaxed),
        type_id: m.charge,
        owner_id: owner,
        corp_id: from.corp_id,
        alliance_id: -1,
        char_id: None,
        mode: MODE_MISSILE,
        radius: size.get::<_, f64>(0) as f32,
        mass: size.get(1),
        max_vel: m.velocity as f32,
        agility: 1.0,
        speed_fraction: 1.0,
        pos: from.pos,
        vel: [0.0; 3],
        goto: target.pos,
        follow_id: target.id,
        follow_range: 0.0,
        free: true,
        global: false,
        massive: false,
        warp: None,
        warp_factor: 0,
        last_ms: now_ms(),
        cloak: 0,
        cloak_until: 0,
        health: [1.0; 3],
        launcher: from.id,
        slim,
    };
    let missile_id = missile.id;
    world.add_ship(system, missile).await.map_err(CallError::Internal)?;
    let signature = attr(&attributes(&db, target.type_id).await?, ATTR_SIGNATURE_RADIUS, target.radius as f64);
    let distance = dist(from.pos, target.pos);
    let delay_ms = std::time::Duration::from_secs_f64((distance / m.velocity.max(1.0)).clamp(0.5, 30.0)).as_millis() as u64;
    let (shooter_id, shooter_pilot) = (from.id, from.char_id);
    let (target_id, target_pilot) = (target.id, target.char_id);
    let (flight_range, aoe_cloud, raw, weapon, attacker) = (m.flight_range, m.aoe_cloud, m.raw, m.weapon, m.attacker);
    let world_ref = Arc::downgrade(&world);
    world.schedule(system, delay_ms, move |s| {
        s.destroy_ball(missile_id);
        let present = s.contains(target_id);
        let reached = distance <= flight_range + 2_000.0;
        s.defer(async move {
            let Some(world) = world_ref.upgrade() else { return };
            let (dealt, quality) = if reached && present {
                let application = (signature / aoe_cloud.max(1.0)).min(1.0);
                (damage(&db, &world, system, target_id, target_pilot, raw.map(|d| d * application), attacker).await.unwrap_or(0.0), 3)
            } else {
                (0.0, 0)
            };
            report_hit(&world, shooter_id, target_id, shooter_pilot, target_pilot, weapon, dealt, quality);
        });
    });
    Ok(())
}

async fn fire_missile(cx: &Ctx, fit: &Fit, module: &Module, charge: i32, target: i64) -> Result<(), CallError> {
    let ship_id = cx.ship_id;
    let Some((ship, t)) = cx.world.with(cx.system_id, move |s| Some((s.ball(ship_id)?.clone(), s.ball(target)?.clone()))).await.flatten() else { return Ok(()) };
    let ch = Key::Charge(module.id);
    let velocity = fit.value(ch, ATTR_MAX_VELOCITY).max(1.0);
    let missile = Missile {
        charge,
        raw: ATTR_DAMAGE.map(|id| fit.value(ch, id) * fit.value(Key::Char, ATTR_MISSILE_DAMAGE_MULTIPLIER).max(0.0)),
        aoe_cloud: fit.value(ch, ATTR_AOE_CLOUD_SIZE),
        velocity,
        flight_range: velocity * fit.value(ch, ATTR_EXPLOSION_DELAY) / 1000.0,
        weapon: module.type_id,
        attacker: Some(cx.char_id),
    };
    launch_missile(cx.db.clone(), cx.world.clone(), cx.system_id, ship, t, cx.char_id, Some(module.id), missile).await
}

/// A module's activation: each cycle uses capacitor and, for weapons, fires; it ends when
/// deactivated, out of capacitor, charges or target, or the repeat count runs out.
async fn run_cycles(cx: Ctx, modules: Vec<Module>, effect: Effect, target: Option<i64>, repeat: i64, stop: Arc<AtomicBool>, first_duration: f64) {
    let master = modules[0].id;
    let started = filetime_now();
    let mut charge_type = None;
    let mut cycles = 0;
    let mut duration_ms = first_duration;
    let mut reason = "";
    if effect.propulsion {
        apply_propulsion(&cx).await;
    }
    loop {
        // Effective attributes as they are now (charges change, modules go offline...).
        let fit = match Fit::load(&cx.db, cx.char_id, cx.ship_id).await {
            Ok(f) => f,
            Err(_) => {
                reason = "fit unavailable";
                break;
            }
        };
        duration_ms = effect.duration_attribute.map(|a| fit.value(Key::Module(master), a)).unwrap_or(duration_ms).max(250.0);
        let need: f64 = modules.iter().map(|m| fit.value(Key::Module(m.id), ATTR_CAPACITOR_NEED)).sum();
        if !drain_capacitor(&cx, &fit, need) {
            reason = "out of capacitor";
            cx.notify("OnEffectMessage", vec![Value::str("NotEnoughCharge"), Value::Dict(vec![])]);
            break;
        }
        if let Some(kind) = effect.weapon {
            let target = target.unwrap_or(0);
            if !targets_of(cx.char_id).contains(&target) {
                reason = "target lost";
                break;
            }
            let mut fired = false;
            for m in &modules {
                let charge = match consume_charge(&cx, m).await {
                    Ok(Some(t)) => t,
                    _ => continue,
                };
                charge_type = Some(charge);
                fired = true;
                let r = match kind {
                    Weapon::Turret => fire_turret(&cx, &fit, m, target).await,
                    Weapon::Missile => fire_missile(&cx, &fit, m, charge, target).await,
                };
                if let Err(e) = r {
                    warn!("weapon {} failed: {}", m.id, match e {
                        CallError::Internal(e) => format!("{e:#}"),
                        CallError::User { msg, .. } => msg,
                    });
                }
            }
            if !fired {
                reason = "out of charges";
                break;
            }
            if cycles == 0 {
                weapon_fx(&cx, &modules, &effect, Some(target), charge_type, true, duration_ms, repeat);
            }
        }
        if let (Some(guid), Some(rock)) = (effect.mining, target) {
            if !targets_of(cx.char_id).contains(&rock) {
                reason = "target lost";
                break;
            }
            if cycles == 0 {
                let beam = modules.iter().map(|m| fire_fx(cx.ship_id, Some((m.id, m.type_id)), guid, Some(rock), None, true, duration_ms, repeat)).collect();
                cx.world.broadcast(cx.system_id, cx.ship_id, beam);
            }
        }
        // Shield boosters land at the start of a cycle, armor and hull repairers at its end.
        let repair = effect.repair.map(|(layer, a)| (layer, fit.value(Key::Module(master), a)));
        if let Some((0, amount)) = repair {
            let _ = repair_player(&cx.db, &cx.world, cx.system_id, cx.ship_id, cx.char_id, 0, amount).await;
        }
        cycles += 1;
        tokio::time::sleep(std::time::Duration::from_secs_f64(duration_ms / 1000.0)).await;
        if let Some((layer @ (1 | 2), amount)) = repair {
            let _ = repair_player(&cx.db, &cx.world, cx.system_id, cx.ship_id, cx.char_id, layer, amount).await;
        }
        // A mining cycle yields at its end.
        if let (Some(_), Some(rock)) = (effect.mining, target) {
            match super::mining::extract(&cx.db, &cx.world, &cx.notify, cx.char_id, cx.ship_id, cx.system_id, &fit, master, rock).await {
                Ok(true) => {}
                Ok(false) => {
                    reason = "nothing more to mine";
                    break;
                }
                Err(e) => {
                    warn!("mining {master}: {}", match e {
                        CallError::Internal(e) => format!("{e:#}"),
                        CallError::User { msg, .. } => msg,
                    });
                    reason = "mining failed";
                    break;
                }
            }
        }
        if stop.load(Ordering::Relaxed) || repeat <= 0 || cycles >= repeat {
            break;
        }
    }
    debug!("module {master} stopped after {cycles} cycles {reason}");
    {
        let mut st = state();
        if st.active.get(&master).is_some_and(|s| Arc::ptr_eq(s, &stop)) {
            st.active.remove(&master);
        }
    }
    ship_effect(&cx, master, &effect, target, false, started, duration_ms, repeat);
    if effect.propulsion {
        apply_propulsion(&cx).await;
    }
    if effect.weapon.is_some() {
        weapon_fx(&cx, &modules, &effect, target, charge_type, false, duration_ms, repeat);
    }
    if let Some(guid) = effect.mining {
        let beam = modules.iter().map(|m| fire_fx(cx.ship_id, Some((m.id, m.type_id)), guid, target, None, false, duration_ms, repeat)).collect();
        cx.world.broadcast(cx.system_id, cx.ship_id, beam);
    }
}

/// Activate(itemID, effectName, targetID, repeat)
async fn activate(c: &mut Client, call: &Call<'_>) -> CallResult {
    let item_id = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
    let effect_name = call.args.get(1).and_then(|v| v.as_text()).map(|s| s.into_owned()).unwrap_or_default();
    let target = call.args.get(2).and_then(|v| v.as_int());
    let repeat = call.args.get(3).and_then(|v| v.as_int()).unwrap_or(0);
    let Some(cx) = ctx(c) else { return Ok(Value::None) };
    let Some(module) = load_items(c, "i.item_id = $1 AND i.location_id = $2", &[&item_id, &cx.ship_id]).await?.into_iter().next() else {
        return Ok(Value::None);
    };
    let row = cx
        .db
        .client
        .query_opt("SELECT effect_id, guid, duration_attribute_id FROM sde.dogma_effects WHERE name = $1", &[&effect_name])
        .await
        .map_err(db_err)?
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("unknown effect {effect_name}")))?;
    let effect = Effect {
        id: row.get::<_, i32>(0) as i64,
        guid: row.get::<_, Option<String>>(1).filter(|g| !g.is_empty()),
        duration_attribute: row.get(2),
        weapon: weapon_kind(&effect_name),
        repair: repair_kind(&effect_name),
        propulsion: matches!(row.get::<_, i32>(0), EFFECT_AFTERBURNER | EFFECT_MICROWARPDRIVE),
        mining: match effect_name.as_str() {
            "miningLaser" => Some("effects.Mining"),
            "miningClouds" => Some("effects.CloudMining"),
            _ => None,
        },
    };
    if state().reloading.get(&item_id).is_some_and(|until| *until > filetime_now()) {
        return Err(user_error("CantActivateReloadingModule", vec![("moduleTypeID".into(), Value::tuple([Value::Int(UE_TYPEID), Value::Int(module.type_id as i64)]))]));
    }
    if effect.weapon.is_some() {
        let Some(t) = target.filter(|t| targets_of(cx.char_id).contains(t)) else {
            return Err(user_error("DeniedActivateTargetNotPresent", vec![]));
        };
        let ship_id = cx.ship_id;
        let (in_warp, own_type, present, victim) = cx.world.with(cx.system_id, move |s| {
            let ship = s.ball(ship_id);
            let target = s.ball(t);
            (ship.is_some_and(|b| b.mode == MODE_WARP), ship.map(|b| b.type_id).unwrap_or(0), target.is_some(), target.and_then(|b| b.char_id))
        }).await.unwrap_or((false, 0, false, None));
        if in_warp {
            return Err(user_error("DeniedActivateInWarp", vec![]));
        }
        if !present {
            return Err(user_error("DeniedActivateTargetNotPresent", vec![]));
        }
        if loaded(&cx.db, cx.ship_id, module.flag_id).await?.is_none_or(|(_, q)| q <= 0) {
            return Err(user_error("NoCharges", vec![("launcher".into(), Value::tuple([Value::Int(UE_TYPEID), Value::Int(module.type_id as i64)]))]));
        }
        // Shooting another pilot is a matter for crimewatch.
        if let Some(victim) = victim {
            super::crimewatch::aggress(&cx.db, &cx.world, cx.system_id, cx.char_id, own_type, module.type_id, victim, t).await?;
        }
    }
    if effect.mining.is_some() {
        let Some(t) = target.filter(|t| targets_of(cx.char_id).contains(t)) else {
            return Err(user_error("DeniedActivateTargetNotPresent", vec![]));
        };
        if !super::mining::is_rock(t) {
            return Err(user_error("CustomNotify", vec![("notify".into(), Value::unicode("只能对小行星、冰矿或气云使用采矿设备。"))]));
        }
        let ship_id = cx.ship_id;
        let gap = cx.world.with(cx.system_id, move |s| {
            s.ball(ship_id).zip(s.ball(t)).map(|(a, b)| dist(a.pos, b.pos) - a.radius as f64 - b.radius as f64)
        }).await.flatten();
        let fit = Fit::load(&cx.db, cx.char_id, cx.ship_id).await?;
        let range = fit.value(Key::Module(item_id), ATTR_MAX_RANGE);
        if gap.is_none_or(|g| g > range) {
            return Err(user_error("CustomNotify", vec![("notify".into(), Value::unicode(format!("目标超出了采矿设备的范围（{:.1} 公里）。", range / 1000.0)))]));
        }
    }
    if state().active.contains_key(&item_id) {
        return Ok(Value::None);
    }
    let fit = Fit::load(&cx.db, cx.char_id, cx.ship_id).await?;
    let duration_ms = effect.duration_attribute.map(|a| fit.value(Key::Module(item_id), a)).unwrap_or(1000.0).max(250.0);
    // The group fires together.
    let mut modules = vec![Module { id: item_id, type_id: module.type_id, flag: module.flag_id }];
    if effect.weapon.is_some() {
        for slave in bank(&cx.db, cx.ship_id, item_id).await?.into_iter().skip(1) {
            if let Some(m) = load_items(c, "i.item_id = $1 AND i.location_id = $2", &[&slave, &cx.ship_id]).await?.into_iter().next() {
                modules.push(Module { id: m.item_id, type_id: m.type_id, flag: m.flag_id });
            }
        }
    }
    let stop = Arc::new(AtomicBool::new(false));
    state().active.insert(item_id, stop.clone());
    ship_effect(&cx, item_id, &effect, target, true, filetime_now(), duration_ms, repeat);
    info!(user = c.login.user_id, "module {item_id} activated {effect_name} on {target:?} (x{}, {duration_ms:.0} ms, repeat {repeat})", modules.len());
    tokio::spawn(run_cycles(cx, modules, effect, target, repeat, stop, duration_ms));
    Ok(Value::None)
}

/// Put a fitted module online or offline in space: remember it, and tell godma (the online
/// effect's state drives the HUD button) and the client's dogma (isOnline).
async fn set_online(c: &mut Client, item_id: i64, online: bool) -> CallResult {
    let Some(cx) = ctx(c) else { return Ok(Value::None) };
    let changed = cx
        .db
        .client
        .execute("UPDATE game.items SET online = $3 WHERE item_id = $1 AND location_id = $2 AND online <> $3", &[&item_id, &cx.ship_id, &online])
        .await
        .map_err(db_err)?;
    if changed == 0 {
        return Ok(Value::None);
    }
    if !online {
        if let Some(stop) = state().active.get(&item_id) {
            stop.store(true, Ordering::Relaxed);
        }
    }
    let effect = Effect { id: EFFECT_ONLINE, guid: None, duration_attribute: None, weapon: None, repair: None, propulsion: false, mining: None };
    ship_effect(&cx, item_id, &effect, None, online, filetime_now(), -1.0, -1);
    let change = attribute_change(cx.char_id, Value::Int(item_id), ATTR_IS_ONLINE, Value::Int(online as i64), Value::Int(!online as i64));
    cx.notify("OnModuleAttributeChanges", vec![Value::list([change])]);
    info!(user = c.login.user_id, "module {item_id} {}", if online { "online" } else { "offline" });
    Ok(Value::None)
}

/// Show a module running one cycle (analyzers): start now, stop after `duration_ms`.
pub(super) fn pulse_effect(c: &mut Client, module: i64, effect_id: i64, target: Option<i64>, duration_ms: f64) {
    let Some(cx) = ctx(c) else { return };
    let effect = Effect { id: effect_id, guid: None, duration_attribute: None, weapon: None, repair: None, propulsion: false, mining: None };
    let started = filetime_now();
    ship_effect(&cx, module, &effect, target, true, started, duration_ms, 0);
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs_f64(duration_ms.max(0.0) / 1000.0)).await;
        ship_effect(&cx, module, &effect, target, false, started, duration_ms, 0);
    });
}

/// Deactivate(itemID, effectName): stop after the current cycle.
fn deactivate(call: &Call<'_>) -> CallResult {
    let item_id = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
    if let Some(stop) = state().active.get(&item_id) {
        stop.store(true, Ordering::Relaxed);
    }
    Ok(Value::None)
}

// ---------------------------------------------------------------- NPCs

/// An NPC's fighting attributes (entity attributes on its type).
struct NpcStats {
    /// (shield boost, armor repair) amount per repair cycle, and the cycle (ms).
    repairs: (f64, f64, f64),
    fly_range: f64,
    aggro_range: f64,
    rate_of_fire_ms: f64,
    optimal: f64,
    falloff: f64,
    damage: [f64; 4],
    missile: Option<(i32, f64, HashMap<i32, f64>)>,
    fx: Option<String>,
}

async fn npc_stats(db: &Db, type_id: i32) -> Result<NpcStats, CallError> {
    let a = attributes(db, type_id).await?;
    let multiplier = attr(&a, ATTR_DAMAGE_MULTIPLIER, 1.0);
    let fx: Option<String> = db
        .client
        .query_opt(
            "SELECT e.guid FROM sde.type_effects te JOIN sde.dogma_effects e USING (effect_id)
             WHERE te.type_id = $1 AND e.effect_category = 2 AND e.guid LIKE 'effects.%' AND e.name <> 'missileLaunching' LIMIT 1",
            &[&type_id],
        )
        .await
        .map_err(db_err)?
        .map(|r| r.get(0));
    let missile_type = attr(&a, ATTR_ENTITY_MISSILE_TYPE, 0.0) as i32;
    let missile = if missile_type > 0 {
        let charge = attributes(db, missile_type).await?;
        Some((missile_type, attr(&a, ATTR_MISSILE_DAMAGE_MULTIPLIER, 1.0), charge))
    } else {
        None
    };
    let optimal = attr(&a, ATTR_MAX_RANGE, 10_000.0);
    Ok(NpcStats {
        repairs: (
            attr(&a, ATTR_ENTITY_SHIELD_BOOST_AMOUNT, 0.0),
            attr(&a, ATTR_ENTITY_ARMOR_REPAIR_AMOUNT, 0.0),
            attr(&a, ATTR_ENTITY_ARMOR_REPAIR_DURATION, 0.0).max(attr(&a, ATTR_ENTITY_SHIELD_BOOST_DURATION, 0.0)).max(5_000.0),
        ),
        fly_range: attr(&a, ATTR_ENTITY_FLY_RANGE, 7_500.0).max(1_000.0),
        aggro_range: attr(&a, ATTR_ENTITY_ATTACK_RANGE, 0.0).max(optimal * 3.0).max(60_000.0),
        rate_of_fire_ms: attr(&a, ATTR_RATE_OF_FIRE, 4_000.0).max(1_000.0),
        optimal,
        falloff: attr(&a, ATTR_FALLOFF, 5_000.0),
        damage: ATTR_DAMAGE.map(|id| attr(&a, id, 0.0) * multiplier),
        missile,
        fx,
    })
}

/// Attach a brain to a spawned NPC; its effects run on the system's worker.
pub async fn spawn_npc(db: Db, world: Arc<World>, system: i64, id: i64, type_id: i32, owner: Option<i64>) -> Result<(), CallError> {
    let stats = match npc_stats(&db, type_id).await {
        Ok(stats) => Arc::new(stats),
        Err(error) => {
            world.post(system, move |s| { s.remove_ball(id); });
            return Err(error);
        }
    };
    let config = NpcConfig {
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

async fn npc_shot(db: &Db, world: &Arc<World>, system: i64, id: i64, type_id: i32, stats: &NpcStats, me: Ball, victim: Ball) -> Result<(), CallError> {
    let ship = victim.id;
    let gap = (dist(me.pos, victim.pos) - me.radius as f64 - victim.radius as f64).max(0.0);
    if let Some((charge, multiplier, c)) = &stats.missile {
        let (charge, multiplier) = (*charge, *multiplier);
        let velocity = attr(c, ATTR_MAX_VELOCITY, 3_000.0).max(1.0);
        let flight = velocity * attr(c, ATTR_EXPLOSION_DELAY, 5_000.0) / 1000.0;
        if gap <= flight {
            let missile = Missile {
                charge,
                raw: ATTR_DAMAGE.map(|a| attr(c, a, 0.0) * multiplier),
                aoe_cloud: attr(c, ATTR_AOE_CLOUD_SIZE, 40.0),
                velocity,
                flight_range: flight,
                weapon: type_id,
                attacker: None,
            };
            launch_missile(db.clone(), world.clone(), system, me.clone(), victim.clone(), me.owner_id, None, missile).await?;
        }
    }
    if stats.damage.iter().any(|d| *d > 0.0) && gap <= stats.optimal + stats.falloff * 3.0 {
        if let Some(guid) = stats.fx.as_deref() {
            let fx = fire_fx(id, None, guid, Some(ship), None, true, stats.rate_of_fire_ms, 1);
            world.broadcast(system, id, vec![fx]);
        }
        let multiplier = turret_roll(gap, stats.optimal, stats.falloff);
        let dealt = if multiplier > 0.0 { damage(db, world, system, ship, victim.char_id, stats.damage.map(|d| d * multiplier), None).await? } else { 0.0 };
        report_hit(world, id, ship, me.char_id, victim.char_id, type_id, dealt, hit_quality(multiplier));
    }
    Ok(())
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let ids = |v: Option<&Value>| -> Vec<i64> { v.and_then(|v| v.items()).unwrap_or_default().iter().filter_map(|x| x.as_int()).collect() };
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
            drop_targets(c, &ids(call.args.first()));
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
        // Data and relic analyzers open the hacking minigame on their target.
        "Activate" if call.args.get(1).and_then(|v| v.as_text()).as_deref() == Some("doHacking") => {
            let module = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
            super::hacking::start(c, module, call.args.get(2).and_then(|v| v.as_int())).await
        }
        // godma runs onlining in space through Activate/Deactivate(moduleID, 'online').
        "Activate" | "Deactivate" if call.args.get(1).and_then(|v| v.as_text()).as_deref() == Some("online") => {
            set_online(c, call.args.first().and_then(|v| v.as_int()).unwrap_or(0), call.method == "Activate").await
        }
        "Activate" => activate(c, call).await,
        "Deactivate" => deactivate(call),
        _ => return None,
    })
}


/// A damaged NPC boosts its shield and repairs its armor every repair cycle.
fn npc_repair(world: &World, system: i64, id: i64, stats: &NpcStats) {
    let (shield, armor, _) = stats.repairs;
    if shield <= 0.0 && armor <= 0.0 {
        return;
    }
    let health = {
        let mut st = state();
        let Some(hp) = st.hp.get_mut(&id) else { return };
        if hp.now[0] >= hp.max[0] && hp.now[1] >= hp.max[1] {
            return;
        }
        hp.now[0] = (hp.now[0] + shield).min(hp.max[0]);
        hp.now[1] = (hp.now[1] + armor).min(hp.max[1]);
        [0, 1, 2].map(|i| if hp.max[i] > 0.0 { (hp.now[i] / hp.max[i]).clamp(0.0, 1.0) } else { 0.0 })
    };
    world.set_health(system, id, health);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_quality_bands() {
        assert_eq!(hit_quality(0.0), 0);
        assert_eq!(hit_quality(0.5), 1);
        assert_eq!(hit_quality(1.1), 4);
        assert_eq!(hit_quality(3.0), 6);
    }

    #[test]
    fn weapon_effects() {
        assert!(weapon_kind("projectileFired") == Some(Weapon::Turret));
        assert!(weapon_kind("useMissiles") == Some(Weapon::Missile));
        assert!(weapon_kind("online").is_none());
    }
}
