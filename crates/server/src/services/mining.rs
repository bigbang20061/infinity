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
//!
//! Reprocessing (the station's bound reprocessingSvc) turns items into their
//! sde.inv_type_materials per portion: a station yield of 50%, raised by Reprocessing,
//! Reprocessing Efficiency and the item's own processing skill, less a 5% tax in ISK.
//! Output that exceeds a single stack's capacity is refused without consuming inputs.

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

/// (typeID, volume) candidates of a field: ores with their +5% / +10% grades.
async fn field_types(db: &Db, field: Field, security: f64, jspace: bool) -> Result<Vec<(i32, f64)>, CallError> {
    let (names, groups) = field_groups(field, security, jspace);
    let names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
    let rows = if groups {
        db
            .query(
                "SELECT t.type_id, t.volume::float8 FROM sde.inv_types t JOIN sde.inv_groups g ON g.group_id = t.group_id
                 WHERE g.category_id = 25 AND g.name = ANY($1) AND t.published
                   AND (t.name = g.name OR t.name = g.name || ' II-Grade' OR t.name = g.name || ' III-Grade')",
                &[&names],
            )
            .await
    } else {
        db.query("SELECT type_id, volume::float8 FROM sde.inv_types WHERE name = ANY($1) AND published", &[&names]).await
    }
    .map_err(db_err)?;
    Ok(rows.iter().map(|r| (r.get(0), r.get::<_, Option<f64>>(1).unwrap_or(1.0).max(0.01))).collect())
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
    let types = field_types(db, field, security, jspace).await?;
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
        let id: i64 = db
            .query_one(
                "INSERT INTO game.asteroids (system_id, field_id, type_id, quantity, original_quantity, radius, x, y, z)
                 VALUES ($1, $2, $3, $4, $4, $5, $6, $7, $8) RETURNING asteroid_id",
                &[&system, &field_id, &type_id, &quantity, &radius, &pos[0], &pos[1], &pos[2]],
            )
            .await
            .map_err(db_err)?
            .get(0);
        state().rocks.insert(id, Rock { system, field: field_id, type_id, quantity });
        world.add_ship(system, rock_ball(id, type_id, pos, radius)).await.map_err(CallError::Internal)?;
        ids.push(id);
    }
    Ok(ids)
}

/// (security, wormhole space) of a system.
async fn system_band(db: &Db, system: i64) -> Result<(f64, bool), CallError> {
    let r = db
        .query_one("SELECT COALESCE(security, 0)::float8 FROM sde.map_solar_systems WHERE solar_system_id = $1", &[&(system as i32)])
        .await
        .map_err(db_err)?;
    Ok((r.get(0), (31_000_000..32_000_000).contains(&system)))
}

/// Bring a system's rocks into space (once per run): the stored ones, and fields for
/// belts that have never been grown or have grown back.
pub async fn ensure_fields(db: &Db, world: &Arc<World>, system: i64) -> Result<(), CallError> {
    if !state().loaded.insert(system) {
        return Ok(());
    }
    let rows = db
        .query("SELECT asteroid_id, field_id, type_id, quantity, radius, x, y, z FROM game.asteroids WHERE system_id = $1", &[&system])
        .await
        .map_err(db_err)?;
    let mut fields_with_rocks = HashSet::new();
    for r in &rows {
        let (id, field): (i64, i64) = (r.get(0), r.get(1));
        fields_with_rocks.insert(field);
        state().rocks.insert(id, Rock { system, field, type_id: r.get(2), quantity: r.get(3) });
        let ball = rock_ball(id, r.get(2), [r.get(5), r.get(6), r.get(7)], r.get::<_, Option<f32>>(4).unwrap_or(2_000.0));
        world.add_ship(system, ball).await.map_err(CallError::Internal)?;
    }
    let belts = db
        .query("SELECT item_id::bigint, x, y, z FROM sde.map_celestials WHERE solar_system_id = $1 AND group_id = $2", &[&(system as i32), &GROUP_ASTEROID_BELT])
        .await
        .map_err(db_err)?;
    if belts.is_empty() {
        return Ok(());
    }
    let (security, jspace) = system_band(db, system).await?;
    let now = filetime_now();
    for b in belts {
        let belt: i64 = b.get(0);
        if fields_with_rocks.contains(&belt) {
            continue;
        }
        let depleted: Option<Option<i64>> = db
            .query_opt("SELECT depleted_at FROM game.asteroid_fields WHERE field_id = $1", &[&belt])
            .await
            .map_err(db_err)?
            .map(|r| r.get(0));
        let regrow_at = depleted.flatten().map(|t| t + BELT_REGROW_SECS * FILETIME_SECOND);
        if regrow_at.is_some_and(|t| t > now) {
            world.system(system).await.map_err(CallError::Internal)?;
            schedule_regrow(db.clone(), world.clone(), system, belt, [b.get(1), b.get(2), b.get(3)], regrow_at.unwrap_or(now));
            continue;
        }
        grow_belt(db, world, system, belt, [b.get(1), b.get(2), b.get(3)], security, jspace).await?;
    }
    Ok(())
}

async fn grow_belt(db: &Db, world: &World, system: i64, belt: i64, center: V3, security: f64, jspace: bool) -> Result<(), CallError> {
    let ids = grow_field(db, world, system, belt, center, Field::Ore, security, jspace).await?;
    db
        .execute(
            "INSERT INTO game.asteroid_fields (field_id, system_id, grown_at) VALUES ($1, $2, $3)
             ON CONFLICT (field_id) DO UPDATE SET grown_at = EXCLUDED.grown_at, depleted_at = NULL",
            &[&belt, &system, &filetime_now()],
        )
        .await
        .map_err(db_err)?;
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
                    let (security, jspace) = system_band(&db, system).await?;
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
    db.execute("DELETE FROM game.asteroids WHERE asteroid_id = $1", &[&id]).await.map_err(db_err)?;
    let field_empty = !state().rocks.values().any(|r| r.field == rock.field);
    if !field_empty {
        return Ok(());
    }
    let belt = db
        .query_opt("SELECT x, y, z FROM sde.map_celestials WHERE item_id = $1 AND group_id = $2", &[&rock.field, &GROUP_ASTEROID_BELT])
        .await
        .map_err(db_err)?;
    if let Some(b) = belt {
        let now = filetime_now();
        db.execute("UPDATE game.asteroid_fields SET depleted_at = $2 WHERE field_id = $1", &[&rock.field, &now]).await.map_err(db_err)?;
        schedule_regrow(db.clone(), world.clone(), rock.system, rock.field, [b.get(0), b.get(1), b.get(2)], now + BELT_REGROW_SECS * FILETIME_SECOND);
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
async fn store(db: &Db, notify: &NotifyTx, owner: i64, ship: i64, flag: i32, type_id: i32, quantity: i32) -> Result<(), CallError> {
    let stack = db
        .query_opt(
            "SELECT item_id, quantity FROM game.items WHERE location_id = $1 AND flag_id = $2 AND type_id = $3 AND owner_id = $4 AND NOT singleton LIMIT 1",
            &[&ship, &flag, &type_id, &owner],
        )
        .await
        .map_err(db_err)?;
    let (item_id, total, change) = match stack {
        Some(r) => {
            let (id, old): (i64, i32) = (r.get(0), r.get(1));
            db.execute("UPDATE game.items SET quantity = quantity + $2 WHERE item_id = $1", &[&id, &quantity]).await.map_err(db_err)?;
            (id, old + quantity, (IX_STACK_SIZE, Value::Int(old as i64)))
        }
        None => {
            let id: i64 = db
                .query_one(
                    "INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity) VALUES ($1, $2, $3, $4, $5) RETURNING item_id",
                    &[&type_id, &owner, &ship, &flag, &quantity],
                )
                .await
                .map_err(db_err)?
                .get(0);
            (id, quantity, (IX_LOCATION_ID, Value::Int(0)))
        }
    };
    let t = db
        .query_one("SELECT COALESCE(group_id, 0)::bigint, COALESCE(category_id, 0)::bigint FROM sde.inv_types WHERE type_id = $1", &[&type_id])
        .await
        .map_err(db_err)?;
    let item = crate::dao::items::loading::Item {
        item_id,
        type_id,
        owner_id: owner,
        location_id: ship,
        flag_id: flag,
        quantity: total,
        singleton: false,
        group_id: t.get(0),
        category_id: t.get(1),
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
        let gap = dist(me.pos, it.pos) - me.radius as f64 - it.radius as f64;
        let pilots: Vec<i64> = s.pilots().filter_map(|b| b.char_id.filter(|c| *c != char_id)).collect();
        Some((gap, pilots))
    }).await.flatten() else { return Ok(false) };
    if gap > fit.value(Key::Module(module), ATTR_MAX_RANGE) {
        tell(notify, messages::TARGET_OUTSIDE_MINING_RANGE);
        return Ok(false);
    }
    let unit_volume: f64 = db
        .query_one("SELECT COALESCE(volume, 1)::float8 FROM sde.inv_types WHERE type_id = $1", &[&rock.type_id])
        .await
        .map_err(db_err)?
        .get::<_, f64>(0)
        .max(0.01);
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
    let used: f64 = db
        .query_one(
            "SELECT COALESCE(sum(i.quantity * COALESCE(t.volume, 0)), 0)::float8 FROM game.items i JOIN sde.inv_types t USING (type_id)
             WHERE i.location_id = $1 AND i.flag_id = $2",
            &[&ship, &flag],
        )
        .await
        .map_err(db_err)?
        .get(0);
    let room = ((capacity - used) / unit_volume).floor().max(0.0) as i64;
    let taken = units.min(room);
    if taken > 0 {
        store(db, notify, char_id, ship, flag, rock.type_id, taken as i32).await?;
    }
    let left = (rock.quantity - taken - wasted_units).max(0);
    if let Some(r) = state().rocks.get_mut(&target) {
        r.quantity = left;
    }
    db.execute("UPDATE game.asteroids SET quantity = $2 WHERE asteroid_id = $1", &[&target, &left]).await.map_err(db_err)?;
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

/// miningScanMgr.perform_scan() -> [(asteroidID, typeID, quantity)] within reach.
async fn perform_scan(c: &Client) -> Value {
    let (Some(system), Some(ship)) = (c.session.int("solarsystemid"), c.session.int("shipid")) else { return Value::None };
    let rocks: Vec<(i64, i32, i64)> = state().rocks.iter().filter(|(_, r)| r.system == system).map(|(id, r)| (*id, r.type_id, r.quantity)).collect();
    c.world.with(system, move |s| {
        let me = s.ball(ship)?;
        Some(Value::List(
            rocks
                .into_iter()
                .filter(|(id, _, _)| s.ball(*id).is_some_and(|b| dist(b.pos, me.pos) <= SCAN_RANGE))
                .map(|(id, type_id, quantity)| Value::tuple([Value::Int(id), Value::Int(type_id as i64), Value::Int(quantity)]))
                .collect(),
        ))
    }).await.flatten().unwrap_or(Value::None)
}

// ---------------------------------------------------------------- reprocessing

/// The pilot's yield multiplier for reprocessing `type_id`: ores and ice train
/// Reprocessing (+3%), Reprocessing Efficiency (+2%) and their processing skill (+2%) per
/// level; everything else Scrapmetal Processing (+2%).
async fn skill_multiplier(c: &Client, char_id: i64, type_id: i32) -> Result<f64, CallError> {
    let own_skill: Option<f64> = c
        .db
        .query_opt("SELECT value FROM sde.type_attributes WHERE type_id = $1 AND attribute_id = $2", &[&type_id, &ATTR_PROCESSING_SKILL])
        .await
        .map_err(db_err)?
        .map(|r| r.get(0));
    let skills = c
        .db
        .query("SELECT type_id, level FROM game.skills WHERE character_id = $1", &[&char_id])
        .await
        .map_err(db_err)?;
    let level = |skill: i32| skills.iter().find(|r| r.get::<_, i32>(0) == skill).map(|r| r.get::<_, i32>(1) as f64).unwrap_or(0.0);
    Ok(match own_skill {
        Some(s) => (1.0 + 0.03 * level(SKILL_REPROCESSING)) * (1.0 + 0.02 * level(SKILL_REPROCESSING_EFFICIENCY)) * (1.0 + 0.02 * level(s as i32)),
        None => 1.0 + 0.02 * level(SKILL_SCRAPMETAL),
    })
}

struct Quote {
    type_id: i32,
    /// Units left over (less than a portion).
    leftover: i32,
    /// (typeID, received, lost, ISK tax)
    materials: Vec<(i32, i64, i64, f64)>,
}

fn output_overflow() -> CallError {
    notify_error(messages::REPROCESS_OUTPUT_STACK_LIMIT)
}

fn add_output(total: i64, amount: i64) -> Result<i64, CallError> {
    total.checked_add(amount).ok_or_else(output_overflow)
}

fn output_stack_quantity(current: Option<i32>, amount: i64) -> Result<i32, CallError> {
    let amount = i32::try_from(amount).map_err(|_| output_overflow())?;
    current.unwrap_or(0).checked_add(amount).ok_or_else(output_overflow)
}

async fn quote(c: &Client, char_id: i64, item: &crate::dao::items::loading::Item) -> Result<Option<Quote>, CallError> {
    let portion: i32 = c
        .db
        .query_one("SELECT GREATEST(COALESCE(portion_size, 1), 1) FROM sde.inv_types WHERE type_id = $1", &[&item.type_id])
        .await
        .map_err(db_err)?
        .get(0);
    let quantity = if item.singleton { 1 } else { item.quantity };
    let batches = quantity / portion;
    if batches == 0 {
        return Ok(None);
    }
    let materials = c
        .db
        .query(
            "SELECT m.material_type_id, m.quantity, COALESCE(p.average, 0)::float8 FROM sde.inv_type_materials m
             LEFT JOIN game.retail_prices p ON p.type_id = m.material_type_id WHERE m.type_id = $1",
            &[&item.type_id],
        )
        .await
        .map_err(db_err)?;
    if materials.is_empty() {
        return Ok(None);
    }
    let efficiency = (STATION_EFFICIENCY * skill_multiplier(c, char_id, item.type_id).await?).min(1.0);
    let mut result = Vec::new();
    for r in &materials {
        let total = r.get::<_, Option<i64>>(1).unwrap_or(0).checked_mul(batches as i64).ok_or_else(output_overflow)?;
        let received = (total as f64 * efficiency).floor() as i64;
        if total > 0 {
            result.push((r.get::<_, i32>(0), received, total - received, received as f64 * r.get::<_, f64>(2) * STATION_TAX));
        }
    }
    let materials = result;
    Ok(Some(Quote { type_id: item.type_id, leftover: quantity % portion, materials }))
}

fn quote_value(item_id: i64, q: &Quote) -> Value {
    let recoverables = q
        .materials
        .iter()
        .map(|(t, received, lost, tax)| {
            keyval([("typeID", Value::Int(*t as i64)), ("client", Value::Int(*received)), ("unrecoverable", Value::Int(*lost)), ("iskCost", Value::Float(*tax))])
        })
        .collect();
    keyval([
        ("itemID", Value::Int(item_id)),
        ("typeID", Value::Int(q.type_id as i64)),
        ("leftOvers", Value::Int(q.leftover as i64)),
        ("recoverables", Value::List(recoverables)),
    ])
}

/// The pilot's items to reprocess: in its hangar at this station, or its ship here.
async fn reprocessable(c: &Client, ids: &[i64]) -> Result<Vec<crate::dao::items::loading::Item>, CallError> {
    let char_id = c.session.int("charid").unwrap_or(0);
    let station = c.session.int("stationid").unwrap_or(0);
    let ship = c.session.int("shipid").unwrap_or(0);
    crate::dao::items::loading::load_items(&c.db, crate::dao::items::loading::ItemQuery::IdsOwnerLocations(ids, char_id, station, ship)).await.map_err(db_err)
}

/// Bound reprocessingSvc (Moniker('reprocessingSvc', stationID)).
async fn reprocessing(c: &mut Client, call: &Call<'_>) -> CallResult {
    let char_id = c.session.int("charid").unwrap_or(0);
    match call.method {
        // GetQuotes(itemIDs, activeShipID) -> (stationTax, {typeID|None: stationEfficiency}, {itemID: quote})
        "GetQuotes" => {
            let mut quotes = vec![];
            for item in reprocessable(c, &value_id_list(call.args.first())).await? {
                if let Some(q) = quote(c, char_id, &item).await? {
                    quotes.push((Value::Int(item.item_id), quote_value(item.item_id, &q)));
                }
            }
            Ok(Value::tuple([Value::Float(STATION_TAX), Value::Dict(vec![(Value::None, Value::Float(STATION_EFFICIENCY))]), Value::Dict(quotes)]))
        }
        "GetStationEfficiencyForTypeID" => Ok(Value::Float(STATION_EFFICIENCY)),
        "GetCharRefiningYieldPercentageForType" => {
            let type_id = call.args.get(1).and_then(|v| v.as_int()).unwrap_or(0) as i32;
            Ok(Value::Float(skill_multiplier(c, char_id, type_id).await?))
        }
        // Reprocess(itemIDs, fromLocationID, ownerID, outputLocationID, outputFlagID)
        //   -> (reprocessed itemIDs, {typeID: quantity})
        "Reprocess" => {
            let station = c.session.int("stationid").ok_or_else(|| notify_error(messages::MUST_DOCK_TO_REPROCESS))?;
            let out_location = call.args.get(3).and_then(|v| v.as_int()).unwrap_or(station);
            let out_flag = call.args.get(4).and_then(|v| v.as_int()).map(|f| f as i32).unwrap_or(FLAG_HANGAR);
            let items = reprocessable(c, &value_id_list(call.args.first())).await?;
            let mut sources = Vec::new();
            let mut output = BTreeMap::<i32, i64>::new();
            let mut tax = 0.0;
            for item in items {
                let Some(q) = quote(c, char_id, &item).await? else { continue };
                for (type_id, received, _, isk) in &q.materials {
                    let total = output.entry(*type_id).or_default();
                    *total = add_output(*total, *received)?;
                    tax += isk;
                }
                sources.push((item, q.leftover));
            }
            let plan = Arc::new((sources, output));
            let db = c.db.clone();
            let plan = db.transaction(
                |tx, outbox| {
                    let plan = Arc::clone(&plan);
                    Box::pin(async move {
                    use crate::dao::items::quantities::{apply, QuantityChange};
                    let mut changes = BTreeMap::new();
                    for (item, leftover) in &plan.0 {
                        changes.insert(item.item_id, QuantityChange::new(item, (*leftover > 0).then_some(*leftover)));
                    }
                    let mut inserts = BTreeMap::new();
                    for (&type_id, &amount) in &plan.1 {
                        if amount <= 0 { continue; }
                        let quantity = output_stack_quantity(None, amount)?;
                        let candidates = crate::dao::items::loading::load_items(tx, crate::dao::items::loading::ItemQuery::LocationFlagTypeOwnerStack(out_location, out_flag, type_id, char_id)).await.map_err(db_err)?;
                        let stack = candidates.into_iter().next();
                        if let Some(item) = stack {
                            let current = changes.get(&item.item_id).map_or(Some(item.quantity), |change| change.quantity);
                            let final_quantity = output_stack_quantity(current, amount)?;
                            changes.entry(item.item_id).and_modify(|change| change.quantity = Some(final_quantity))
                                .or_insert_with(|| QuantityChange::new(&item, Some(final_quantity)));
                        } else {
                            inserts.insert(type_id, quantity);
                        }
                    }
                    if !apply(tx, &changes).await.map_err(db_err)? { return Err(notify_error(messages::INSUFFICIENT_ITEM_QUANTITY)); }
                    for (&id, change) in &changes {
                        if change.quantity.is_some() {
                            super::inventory::notify_item_change(tx, outbox, id, vec![(IX_STACK_SIZE, Value::Int(change.expected_quantity as i64))]).await?;
                        } else {
                            let item = plan.0.iter().find(|(item, _)| item.item_id == id).expect("removed item is a source");
                            super::inventory::notify_item_removed(outbox, item.0.clone());
                        }
                    }
                    for (type_id, quantity) in inserts {
                        let id: i64 = tx.query_one(
                            "INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity) VALUES ($1, $2, $3, $4, $5) RETURNING item_id",
                            &[&type_id, &char_id, &out_location, &out_flag, &quantity],
                        ).await.map_err(db_err)?.get(0);
                        super::inventory::notify_item_change(tx, outbox, id, vec![(IX_LOCATION_ID, Value::Int(0)), (IX_FLAG, Value::Int(out_flag as i64))]).await?;
                    }
                    if tax > 0.0 {
                        let row = tx.query_opt(
                            "UPDATE game.characters SET balance = balance - $2::float8::numeric WHERE character_id = $1 AND balance >= $2::float8::numeric RETURNING balance::float8",
                            &[&char_id, &tax],
                        ).await.map_err(db_err)?.ok_or_else(|| notify_error(messages::INSUFFICIENT_ISK_FOR_REPROCESSING_TAX))?;
                        outbox.notify_client("OnAccountChange", vec![Value::str("cash"), Value::Int(char_id), Value::Float(row.get(0))]);
                    }
                    Ok::<_, CallError>(Arc::clone(&plan))
                })
                },
                |value, outbox| async { crate::db::outbox::dispatch_client(value, outbox, c) },
            ).await?;
            let done: Vec<_> = plan.0.iter().map(|(item, _)| Value::Int(item.item_id)).collect();
            info!(user = c.login.user_id, "reprocessed {} items into {} material types ({tax:.0} ISK tax)", done.len(), plan.1.len());
            Ok(Value::tuple([Value::List(done), Value::Dict(plan.1.iter().map(|(&t, &q)| (Value::Int(t as i64), Value::Int(q))).collect())]))
        }
        _ => Ok(Value::None),
    }
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match (call.service, call.method) {
        ("miningScanMgr", "perform_scan") => Some(Ok(perform_scan(c).await)),
        ("reprocessingSvc", _) if call.bound => Some(reprocessing(c, call).await),
        _ => None,
    }
}

#[cfg(test)] mod tests;
