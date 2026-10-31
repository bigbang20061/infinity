//! Mining: asteroid belts, ore/ice/gas sites' fields, mining lasers, strip miners, ice and
//! gas harvesters, the ship mining scanner and reprocessing at stations.
//!
//! Every asteroid belt of a system gets a field of rocks the first time a pilot enters
//! the system: ores by the system's security band, sized in the thousands of m3 each.
//! Exploration's ore, ice and gas sites get fields of their own (services/exploration).
//! Rocks are stored in game.asteroids and shrink as they are mined; an emptied belt
//! grows back after BELT_REGROW_SECS, scheduled on its system actor. Depletion
//! timestamps persist; timers are restored when the system's fields are loaded.
//!
//! A mining module runs through combat's activation loop (the `miningLaser` /
//! `miningClouds` effects): at the end of each cycle `extract` moves its yield
//! (miningAmount m3, with critical successes and residue waste) from the rock to the
//! mining hold, or the cargo hold of ships without one, and tells the client
//! (OnOreMined -> notifications.oreMinedNotificationAdapter, OnItemChange).
//! Extraction range uses ship and rock positions sampled at the same system-actor time.
//!
//! Reprocessing (the station's bound reprocessingSvc) turns items into their
//! canonical SDE type materials per portion: a station yield of 50%, raised by Reprocessing,
//! Reprocessing Efficiency and the item's own processing skill, less a 5% tax in ISK.
//! Output that exceeds a single stack's capacity is refused without consuming inputs.

mod mining_scan_mgr;
mod reprocessing_svc;
use super::fit::{Fit, Key};
use super::{db_err, keyval, notify_error, rand01, rand_dir, value_id_list, Call, CallError, CallResult};
use crate::client::{filetime_now, unix_ms_from_filetime_ceil, Client};
use crate::consts::{ATTR_CAPACITY, ATTR_MAX_RANGE, FILETIME_SECOND, FLAG_CARGO, FLAG_HANGAR, GROUP_ASTEROID_BELT, IX_FLAG, IX_LOCATION_ID, IX_STACK_SIZE};
use crate::db::Db;
use crate::messages;
use crate::space::{dist, now_ms, Ball, NotifyTx, World, MODE_RIGID, V3};
use marshal::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use tracing::{info, warn};

const ATTR_MINING_AMOUNT: i32 = 77;
const ATTR_MINING_HOLD: i32 = 1556; // generalMiningHoldCapacity
const ATTR_WASTE_MULTIPLIER: i32 = 3153;
const ATTR_WASTE_PROBABILITY: i32 = 3154;
const ATTR_CRIT_CHANCE: i32 = 5967;
const ATTR_CRIT_BONUS: i32 = 5969;
const ATTR_PROCESSING_SKILL: i32 = 790; // reprocessingSkillType
const FLAG_MINING_HOLD: i32 = 134; // flagGeneralMiningHold
const SKILL_REPROCESSING: i32 = 3385;
const SKILL_REPROCESSING_EFFICIENCY: i32 = 3389;
/// SDE invCategories: asteroid resources.
const CATEGORY_ASTEROID: i32 = 25;
const SKILL_SCRAPMETAL: i32 = 12196;
/// Server policy: an emptied belt grows back after two hours.
const BELT_REGROW_SECS: i64 = 7200;
/// NPC station reprocessing: base yield and tax (of the materials' value, in ISK).
const STATION_EFFICIENCY: f64 = 0.5;
const STATION_TAX: f64 = 0.05;
/// The ship mining scanner reaches this far (m).
const SCAN_RANGE: f64 = 500_000.0;


// ---------------------------------------------------------------- fields

/// What a field is made of.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Field {
    Ore,
    Ice,
    Gas,
}

#[derive(Clone)]
struct Rock {
    system: i64,
    field: i64,
    type_id: i32,
    quantity: i64,
}

#[derive(Default)]
struct State {
    rocks: HashMap<i64, Rock>,
    loaded: HashSet<i64>,
    /// Fraction of an ore unit each module has mined toward its next one (m3).
    carry: HashMap<i64, f64>,
}

fn state() -> MutexGuard<'static, State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(Default::default).lock().unwrap()
}

/// Groups (by name) a field draws from: ores by security band, ice by security, gas by
/// space (Fullerite in wormhole space, the boosters' gases elsewhere).
fn field_groups(field: Field, security: f64, jspace: bool) -> (&'static [&'static str], bool) {
    // (names, are they group names (ore) or type names (ice, gas))
    match field {
        Field::Ore if jspace || security <= 0.0 => (&["Gneiss", "Dark Ochre", "Spodumain", "Crokite", "Bistot", "Arkonor", "Hemorphite", "Hedbergite"], true),
        Field::Ore if security < 0.45 => (&["Kernite", "Omber", "Jaspet", "Hemorphite", "Hedbergite", "Plagioclase"], true),
        Field::Ore if security < 0.7 => (&["Veldspar", "Scordite", "Pyroxeres", "Plagioclase", "Omber", "Kernite"], true),
        Field::Ore => (&["Veldspar", "Scordite", "Pyroxeres"], true),
        Field::Ice if security >= 0.45 => (&["Clear Icicle", "Glacial Mass", "Blue Ice", "White Glaze"], false),
        Field::Ice if security > 0.0 => (&["Glare Crust", "Dark Glitter", "Gelidus", "Blue Ice", "Clear Icicle"], false),
        Field::Ice => (&["Krystallos", "Glare Crust", "Dark Glitter", "Gelidus"], false),
        Field::Gas if jspace => (&["Fullerite-C50", "Fullerite-C60", "Fullerite-C70", "Fullerite-C72", "Fullerite-C84", "Fullerite-C28", "Fullerite-C32", "Fullerite-C320", "Fullerite-C540"], false),
        Field::Gas if security >= 0.45 => (&["Amber Mykoserocin", "Golden Mykoserocin", "Viridian Mykoserocin", "Celadon Mykoserocin", "Lime Mykoserocin"], false),
        Field::Gas => (&["Amber Cytoserocin", "Golden Cytoserocin", "Viridian Cytoserocin", "Celadon Cytoserocin", "Lime Cytoserocin", "Vermillion Cytoserocin", "Azure Cytoserocin"], false),
    }
}
fn ore_candidate(name: &str, group: &str, category: Option<i32>, names: &[&str]) -> bool {
    category == Some(CATEGORY_ASTEROID) && names.contains(&group)
        && (name == group || name.strip_prefix(group).is_some_and(|suffix| suffix == " II-Grade" || suffix == " III-Grade"))
}


/// (typeID, volume) candidates of a field: ores with their +5% / +10% grades.
fn field_types(sde: &crate::sde::Sde, field: Field, security: f64, jspace: bool) -> Vec<(i32, f64)> {
    let (names, groups) = field_groups(field, security, jspace);
    sde.types.iter().filter(|t| {
        if t.published != Some(true) { return false; }
        let Some(name) = t.name.as_deref() else { return false };
        if !groups { return names.contains(&name); }
        let Some(group) = t.group_id.and_then(|id| sde.types.group(id)) else { return false };
        let Some(group_name) = group.name.as_deref() else { return false };
        ore_candidate(name, group_name, group.category_id, names)
    }).map(|t| (t.type_id, t.volume.unwrap_or(1.0).max(0.01))).collect()
}

fn rock_ball(id: i64, type_id: i32, pos: V3, radius: f32) -> Ball {
    Ball {
        id,
        type_id,
        owner_id: 1,
        corp_id: 1,
        alliance_id: -1,
        char_id: None,
        signature_radius: None,
        mode: MODE_RIGID,
        radius,
        mass: 1e34,
        max_vel: 0.0,
        agility: 1.0,
        speed_fraction: 0.0,
        pos,
        vel: [0.0; 3],
        old_pos: pos, old_vel: [0.0; 3], segment_target: None, segment_warp: None,
        goto: [0.0; 3],
        follow_id: 0,
        follow_range: 0.0,
        free: false,
        global: false,
        massive: true,
        interactive: false,
        warp: None,
        warp_factor: 0,
        last_ms: now_ms(),
        interpolation_anchor_ms: 0,
        effect_stamp: 0,
        cloak: 0,
        cloak_until: 0,
        health: [1.0; 3],
        launcher: 0,
        slim: vec![],
    }
}

/// A rock's size from its volume (m3): a few km for ore, more for ice and gas clouds.
fn rock_radius(field: Field, volume_m3: f64) -> f32 {
    let r = match field {
        Field::Ore => volume_m3.cbrt() * 150.0,
        Field::Ice => 6_000.0 + volume_m3.cbrt() * 30.0,
        Field::Gas => 12_000.0 + rand01() * 10_000.0,
    };
    r.clamp(800.0, 25_000.0) as f32
}

/// Grow a field of rocks around `center` and put it in space. Returns the rock IDs.
pub async fn grow_field(db: &Db, world: &World, system: i64, field_id: i64, center: V3, field: Field, security: f64, jspace: bool) -> Result<Vec<i64>, CallError> {
    let types = field_types(world.sde(), field, security, jspace);
    if types.is_empty() {
        return Ok(vec![]);
    }
    let count = match field {
        Field::Ore => 15 + (rand01() * 10.0) as usize,
        Field::Ice => 6 + (rand01() * 5.0) as usize,
        Field::Gas => 5 + (rand01() * 4.0) as usize,
    };
    let mut ids = vec![];
    for _ in 0..count {
        let (type_id, unit_volume) = types[(rand01() * types.len() as f64) as usize % types.len()];
        let volume = match field {
            Field::Ore => 4_000.0 + rand01() * 21_000.0,
            Field::Ice => (300.0 + rand01() * 1_200.0) * unit_volume,
            Field::Gas => 3_000.0 + rand01() * 12_000.0,
        };
        let quantity = (volume / unit_volume).round().max(1.0) as i64;
        let radius = rock_radius(field, volume);
        let spread = if field == Field::Gas { 60_000.0 } else { 40_000.0 };
        let v = rand_dir();
        let d = 3_000.0 + rand01() * spread;
        let pos = [center[0] + v[0] * d, center[1] + v[1] * d * 0.3, center[2] + v[2] * d];
        let id = crate::dao::asteroids::insert(db, system, field_id, type_id, quantity, radius, pos).await.map_err(db_err)?;
        state().rocks.insert(id, Rock { system, field: field_id, type_id, quantity });
        world.add_ship(system, rock_ball(id, type_id, pos, radius)).await.map_err(CallError::Internal)?;
        ids.push(id);
    }
    Ok(ids)
}

/// (security, wormhole space) of a system.
fn system_band(sde: &crate::sde::Sde, system: i64) -> Result<(f64, bool), CallError> {
    let row = sde.map.system(system as i32 as i64).ok_or_else(|| CallError::Internal(anyhow::anyhow!("missing solar system {system}")))?;
    Ok((row.security.unwrap_or(0.0), (31_000_000..32_000_000).contains(&system)))
}

/// Bring a system's rocks into space (once per run): the stored ones, and fields for
/// belts that have never been grown or have grown back.
pub async fn ensure_fields(db: &Db, world: &Arc<World>, system: i64) -> Result<(), CallError> {
    if !state().loaded.insert(system) {
        return Ok(());
    }
    let rows = crate::dao::asteroids::load(db, system).await.map_err(db_err)?;
    let mut fields_with_rocks = HashSet::new();
    for r in &rows {
        let (id, field) = (r.id, r.field);
        fields_with_rocks.insert(field);
        state().rocks.insert(id, Rock { system, field, type_id: r.type_id, quantity: r.quantity });
        let ball = rock_ball(id, r.type_id, [r.x, r.y, r.z], r.radius.unwrap_or(2_000.0));
        world.add_ship(system, ball).await.map_err(CallError::Internal)?;
    }
    let belts: Vec<_> = world.sde().map.celestials(system as i32 as i64).filter(|b| b.group_id == Some(GROUP_ASTEROID_BELT as i64)).collect();
    if belts.is_empty() {
        return Ok(());
    }
    let (security, jspace) = system_band(world.sde(), system)?;
    let now = filetime_now();
    for b in belts {
        let belt = b.item_id;
        if fields_with_rocks.contains(&belt) {
            continue;
        }
        let depleted = crate::dao::asteroids::depleted(db, belt).await.map_err(db_err)?;
        let regrow_at = depleted.flatten().map(|t| t + BELT_REGROW_SECS * FILETIME_SECOND);
        if regrow_at.is_some_and(|t| t > now) {
            world.system(system).await.map_err(CallError::Internal)?;
            schedule_regrow(db.clone(), world.clone(), system, belt, [b.x.expect("belt x"), b.y.expect("belt y"), b.z.expect("belt z")], regrow_at.unwrap_or(now));
            continue;
        }
        grow_belt(db, world, system, belt, [b.x.expect("belt x"), b.y.expect("belt y"), b.z.expect("belt z")], security, jspace).await?;
    }
    Ok(())
}

async fn grow_belt(db: &Db, world: &World, system: i64, belt: i64, center: V3, security: f64, jspace: bool) -> Result<(), CallError> {
    let ids = grow_field(db, world, system, belt, center, Field::Ore, security, jspace).await?;
    crate::dao::asteroids::grown(db, belt, system, filetime_now()).await.map_err(db_err)?;
    info!("mining: belt {belt} in {system} grew {} rocks", ids.len());
    Ok(())
}

fn schedule_regrow(db: Db, world: Arc<World>, system: i64, belt: i64, center: V3, at: i64) {
    let deadline_ms = unix_ms_from_filetime_ceil(at);
    let world_ref = Arc::downgrade(&world);
    world.post(system, move |s| {
        s.schedule_at(deadline_ms, move |s| {
            s.defer(async move {
                let Some(world) = world_ref.upgrade() else { return };
                let result = async {
                    let (security, jspace) = system_band(world.sde(), system)?;
                    grow_belt(&db, &world, system, belt, center, security, jspace).await
                };
                if let Err(e) = result.await {
                    warn!("mining: regrowing belt {belt}: {}", match e {
                        CallError::Internal(e) => format!("{e:#}"),
                        CallError::User { msg, .. } => msg,
                    });
                }
            });
        });
    });
}

/// A rock is gone: out of space and the database; an emptied belt starts growing back.
async fn exhaust(db: &Db, world: &Arc<World>, id: i64) -> Result<(), CallError> {
    let Some(rock) = state().rocks.remove(&id) else { return Ok(()) };
    world.remove_ball(rock.system, id).await;
    crate::dao::asteroids::delete(db, id).await.map_err(db_err)?;
    let field_empty = !state().rocks.values().any(|r| r.field == rock.field);
    if !field_empty {
        return Ok(());
    }
    let belt = world.sde().map.celestial(rock.field).filter(|b| b.group_id == Some(GROUP_ASTEROID_BELT as i64));
    if let Some(b) = belt {
        let now = filetime_now();
        crate::dao::asteroids::mark_depleted(db, rock.field, now).await.map_err(db_err)?;
        schedule_regrow(db.clone(), world.clone(), rock.system, rock.field, [b.x.expect("belt x"), b.y.expect("belt y"), b.z.expect("belt z")], now + BELT_REGROW_SECS * FILETIME_SECOND);
        info!("mining: belt {} in {} mined out", rock.field, rock.system);
    }
    Ok(())
}

/// Is this ball a rock that can be mined (and with what kind of module)?
pub fn is_rock(id: i64) -> bool {
    state().rocks.contains_key(&id)
}

// ---------------------------------------------------------------- extraction

/// Put mined ore in a hold, onto a stack of the same type if there is one, and tell the
/// pilot (OnItemChange).
async fn store(db: &Db, sde: &crate::sde::Sde, notify: &NotifyTx, owner: i64, ship: i64, flag: i32, type_id: i32, quantity: i32) -> Result<(), CallError> {
    let stack = crate::dao::items::transfers::stack(db, ship, flag, type_id, owner).await.map_err(db_err)?;
    let (item_id, total, change) = match stack {
        Some(r) => {
            let (id, old) = (r.item_id, r.quantity);
            crate::dao::items::transfers::add(db, id, quantity).await.map_err(db_err)?;
            (id, old + quantity, (IX_STACK_SIZE, Value::Int(old as i64)))
        }
        None => {
            let id = crate::dao::items::transfers::insert(db, type_id, owner, ship, flag, quantity).await.map_err(db_err)?;
            (id, quantity, (IX_LOCATION_ID, Value::Int(0)))
        }
    };
    let t = sde.types.get(type_id).ok_or_else(|| CallError::Internal(anyhow::anyhow!("missing type {type_id}")))?;
    let item = crate::dao::items::loading::Item {
        item_id,
        type_id,
        owner_id: owner,
        location_id: ship,
        flag_id: flag,
        quantity: total,
        singleton: false,
        group_id: t.group_id.unwrap_or(0) as i64,
        category_id: t.category_id.unwrap_or(0) as i64,
        custom_info: None,
    };
    let _ = notify.send(("OnItemChange".into(), vec![item.row(), Value::Dict(vec![(Value::Int(change.0), change.1)]), Value::None]));
    Ok(())
}

fn tell(notify: &NotifyTx, text: &str) {
    let _ = notify.send(("OnRemoteMessage".into(), vec![Value::str(messages::MSG_CUSTOM_NOTIFY), Value::dict([("notify", Value::unicode(text))])]));
}

/// The end of a mining cycle of `module` on rock `target`: move the yield to the hold.
/// Returns whether the module should keep cycling.
#[allow(clippy::too_many_arguments)]
pub async fn extract(db: &Db, world: &Arc<World>, notify: &NotifyTx, char_id: i64, ship: i64, system: i64, fit: &Fit, module: i64, target: i64) -> Result<bool, CallError> {
    let Some(rock) = state().rocks.get(&target).cloned() else { return Ok(false) };
    let Some((gap, pilots)) = world.with(system, move |s| {
        let (me, it) = (s.ball(ship)?, s.ball(target)?);
        let gap = dist(s.sample_position(ship)?, s.sample_position(target)?) - me.radius as f64 - it.radius as f64;
        let pilots: Vec<i64> = s.pilots().filter_map(|b| b.char_id.filter(|c| *c != char_id)).collect();
        Some((gap, pilots))
    }).await.flatten() else { return Ok(false) };
    if gap > fit.value(Key::Module(module), ATTR_MAX_RANGE) {
        tell(notify, messages::TARGET_OUTSIDE_MINING_RANGE);
        return Ok(false);
    }
    let unit_volume = world.sde().types.get(rock.type_id).ok_or_else(|| CallError::Internal(anyhow::anyhow!("missing type {}", rock.type_id)))?.volume.unwrap_or(1.0).max(0.01);
    // Yield this cycle: miningAmount, doubled-up on a critical success, less any residue.
    let mut amount = fit.value(Key::Module(module), ATTR_MINING_AMOUNT);
    let crit_bonus = if rand01() < fit.value(Key::Module(module), ATTR_CRIT_CHANCE) { amount * fit.value(Key::Module(module), ATTR_CRIT_BONUS) } else { 0.0 };
    let wasted = if rand01() < fit.value(Key::Module(module), ATTR_WASTE_PROBABILITY) { amount * fit.value(Key::Module(module), ATTR_WASTE_MULTIPLIER) } else { 0.0 };
    amount += crit_bonus + state().carry.remove(&module).unwrap_or(0.0);
    let mut units = (amount / unit_volume).floor() as i64;
    state().carry.insert(module, amount - units as f64 * unit_volume);
    let wasted_units = ((wasted / unit_volume).floor() as i64).min((rock.quantity - units).max(0));
    units = units.min(rock.quantity);
    // Into the mining hold, or the cargo hold of ships without one.
    let hold = fit.value(Key::Ship, ATTR_MINING_HOLD);
    let (flag, capacity) = if hold > 0.0 { (FLAG_MINING_HOLD, hold) } else { (FLAG_CARGO, fit.value(Key::Ship, ATTR_CAPACITY)) };
    let used: f64 = crate::dao::items::mining::rows(db, ship, flag).await.map_err(db_err)?.iter()
        .filter_map(|item| world.sde().types.get(item.type_id).map(|t| item.quantity as f64 * t.volume.unwrap_or(0.0))).fold(0.0, |total, v| total + v);
    let room = ((capacity - used) / unit_volume).floor().max(0.0) as i64;
    let taken = units.min(room);
    if taken > 0 {
        store(db, world.sde(), notify, char_id, ship, flag, rock.type_id, taken as i32).await?;
    }
    let left = (rock.quantity - taken - wasted_units).max(0);
    if let Some(r) = state().rocks.get_mut(&target) {
        r.quantity = left;
    }
    crate::dao::asteroids::quantity(db, target, left).await.map_err(db_err)?;
    let crit_units = ((crit_bonus / unit_volume).floor() as i64).min(taken);
    let _ = notify.send((
        "OnOreMined".into(),
        vec![Value::dict([
            ("oreType", Value::Int(rock.type_id as i64)),
            ("quantity_added", Value::Int(taken)),
            ("quantity_removed", Value::Int(taken)),
            ("hasRewards", Value::Bool(false)),
            ("amountWasted", Value::Int(wasted_units)),
            ("amountCritBonus", Value::Int(crit_units)),
            ("itemID", Value::Int(target)),
            ("moduleItemID", Value::Int(module)),
        ])],
    ));
    // Others mining the same rock see it shrink.
    for pilot in pilots {
        world.notify_pilot(pilot, "OnOreMinedByOther", vec![Value::Int(target), Value::Int(taken + wasted_units)]);
    }
    if left == 0 {
        exhaust(db, world, target).await?;
        return Ok(false);
    }
    if taken < units {
        tell(notify, if flag == FLAG_MINING_HOLD { messages::MINING_HOLD_FULL } else { messages::CARGO_HOLD_FULL });
        return Ok(false);
    }
    Ok(true)
}



pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match (call.service, call.method) {
        ("miningScanMgr", "perform_scan") => Some(Ok(mining_scan_mgr::perform_scan(c).await)),
        ("reprocessingSvc", _) if call.bound => Some(reprocessing_svc::reprocessing(c, call).await),
        _ => None,
    }
}

#[cfg(test)] mod tests;
