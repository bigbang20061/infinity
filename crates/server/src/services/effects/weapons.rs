//! Turret shots and missile launches using fitted dogma and charge data.
//! Mirrors clientDogmaLocation and michelle; missiles use the existing approximate
//! flight and explosion-radius application rather than native missile physics.
use super::lifecycle::Module;
use crate::services::combat::{attr, groups_health::damage, quantity_change, Ctx, ATTR_DAMAGE};
use crate::services::fit::{Fit, Key};
use crate::services::{db_err, rand01, CallError};
use crate::consts::{ATTR_AOE_CLOUD_SIZE, ATTR_DAMAGE_MULTIPLIER, ATTR_EXPLOSION_DELAY, ATTR_FALLOFF, ATTR_MAX_RANGE, ATTR_MAX_VELOCITY, ATTR_MISSILE_DAMAGE_MULTIPLIER, ATTR_SIGNATURE_RADIUS};
use crate::db::Db;
use crate::db::outbox::{dispatch_world, Outbox};
use crate::space::{dist, ms_to_seconds, now_ms, seconds_to_ms, Ball, World};
use marshal::Value;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

/// Existing missile flight-time clamp.
const MIN_MISSILE_FLIGHT_SECONDS: f64 = 0.5;
/// Existing missile flight-time clamp.
const MAX_MISSILE_FLIGHT_SECONDS: f64 = 30.0;
/// Existing missile reach tolerance.
const MISSILE_REACH_ALLOWANCE_METRES: f64 = 2000.0;
/// Existing turret falloff hit-chance formula.
const TURRET_FALLOFF_CHANCE: f64 = 0.5;
/// Existing turret wrecking-hit probability.
const WRECKING_CHANCE: f64 = 0.01;
/// Existing turret wrecking-hit damage multiplier.
const WRECKING_DAMAGE_MULTIPLIER: f64 = 3.0;
/// Existing turret normal-hit damage formula.
const NORMAL_HIT_MULTIPLIER_OFFSET: f64 = 0.49;
/// Existing combat-log quality IDs and thresholds.
const HIT_MISS: i64 = 0;
/// Existing combat-log grazing quality.
const HIT_GRAZING: i64 = 1;
/// Existing combat-log glancing quality.
const HIT_GLANCING: i64 = 2;
/// Existing combat-log normal quality.
const HIT_NORMAL: i64 = 3;
/// Existing combat-log good quality.
const HIT_GOOD: i64 = 4;
/// Existing combat-log excellent quality.
const HIT_EXCELLENT: i64 = 5;
/// Existing combat-log wrecking quality.
const HIT_WRECKING: i64 = 6;
/// Existing combat-log wrecking threshold.
const HIT_WRECKING_THRESHOLD: f64 = 2.9;
/// Existing combat-log grazing threshold.
const HIT_GRAZING_THRESHOLD: f64 = 0.6;
/// Existing combat-log glancing threshold.
const HIT_GLANCING_THRESHOLD: f64 = 0.8;
/// Existing combat-log normal threshold.
const HIT_NORMAL_THRESHOLD: f64 = 1.0;
/// Existing combat-log good threshold.
const HIT_GOOD_THRESHOLD: f64 = 1.2;

/// Server design decision: missile IDs clear NPCs (9.0e12), exploration sites (9.5e12), probes (9.6e12), and asteroids (9.7e12).
const MISSILE_ID_START: i64 = 9_800_000_000_000;
/// Temporary item IDs for missiles in flight.
static NEXT_MISSILE_ID: AtomicI64 = AtomicI64::new(MISSILE_ID_START);

/// Use one charge from a module; `None` when it is empty.
pub(super) async fn consume_charge(cx: &Ctx, module: &Module, expected: Option<i32>) -> Result<Option<i32>, CallError> {
    let (ship, flag, char_id) = (cx.ship_id, module.flag, cx.char_id);
    let notify = cx.notify.clone();
    cx.db.transaction(
        |tx, outbox: &mut Outbox| {
            let notify = notify.clone();
            Box::pin(async move {
            if !crate::dao::items::ship::lock_reload_ship(tx, ship).await.map_err(db_err)?
                || !crate::dao::items::pending_reloads::for_flags(tx, ship, &[flag]).await.map_err(db_err)?.is_empty() {
                return Ok(None);
            }
            if let Some(expected) = expected {
                if crate::services::combat::targeting_charges::loaded(tx, ship, flag).await?.is_none_or(|(kind, quantity)| kind != expected || quantity <= 0) {
                    return Ok(None);
                }
            }
            let Some(r) = crate::dao::items::combat::consume_charge(tx, ship, flag).await.map_err(db_err)? else { return Ok(None) };
            if r.quantity == 0 {
                crate::dao::items::combat::delete_empty_charge(tx, ship, flag).await.map_err(db_err)?;
            }
            outbox.notify_sender(notify, "OnModuleAttributeChanges", vec![quantity_change(char_id, ship, flag, r.type_id, r.quantity as i64, r.quantity as i64 + 1)]);
            Ok(Some(r.type_id))
            })
        },
        |value, outbox| async move { dispatch_world(value, outbox, &cx.world) },
    ).await
}

/// Confirm an uncharged ancillary cycle did not race a reload or a new charge.
pub(super) async fn empty_charge(cx: &Ctx, module: &Module) -> Result<bool, CallError> {
    let (ship, flag) = (cx.ship_id, module.flag);
    cx.db.transaction(
        |tx, _: &mut Outbox| Box::pin(async move {
            if !crate::dao::items::ship::lock_reload_ship(tx, ship).await.map_err(db_err)?
                || !crate::dao::items::pending_reloads::for_flags(tx, ship, &[flag]).await.map_err(db_err)?.is_empty() {
                return Ok(false);
            }
            Ok(crate::services::combat::targeting_charges::loaded(tx, ship, flag).await?.is_none_or(|(_, quantity)| quantity <= 0))
        }),
        |value, outbox| async move { dispatch_world(value, outbox, &cx.world) },
    ).await
}

/// The game's hit quality for the combat log: 0 miss .. 6 wrecking.
pub(crate) fn hit_quality(multiplier: f64) -> i64 {
    match multiplier {
        m if m <= 0.0 => HIT_MISS,
        m if m >= HIT_WRECKING_THRESHOLD => HIT_WRECKING,
        m if m < HIT_GRAZING_THRESHOLD => HIT_GRAZING,
        m if m < HIT_GLANCING_THRESHOLD => HIT_GLANCING,
        m if m < HIT_NORMAL_THRESHOLD => HIT_NORMAL,
        m if m < HIT_GOOD_THRESHOLD => HIT_GOOD,
        _ => HIT_EXCELLENT,
    }
}

/// A turret's damage multiplier for one shot: miss (0), 0.49..1.49, or a 1% wrecking 3x,
/// with the hit chance falling off past optimal range.
pub(crate) fn turret_roll(gap: f64, optimal: f64, falloff: f64) -> f64 {
    let ratio = (gap - optimal).max(0.0) / falloff.max(1.0);
    let chance = TURRET_FALLOFF_CHANCE.powf(ratio * ratio);
    let roll = rand01();
    if roll >= chance {
        0.0
    } else if roll < WRECKING_CHANCE {
        WRECKING_DAMAGE_MULTIPLIER
    } else {
        roll + NORMAL_HIT_MULTIPLIER_OFFSET
    }
}

/// `OnDamageMessage` for the shooter (`target`) or the victim (`source`).
pub(super) fn damage_message(key: &str, other: i64, weapon: i32, damage: f64, quality: i64) -> Value {
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
pub(crate) fn report_hit(world: &World, shooter: i64, target: i64, shooter_pilot: Option<i64>, target_pilot: Option<i64>, weapon: i32, damage: f64, quality: i64) {
    if let Some(ch) = shooter_pilot {
        world.notify_pilot(ch, "OnDamageMessage", vec![damage_message("target", target, weapon, damage, quality)]);
    }
    if let Some(ch) = target_pilot {
        world.notify_pilot(ch, "OnDamageMessage", vec![damage_message("source", shooter, weapon, damage, quality)]);
    }
}

/// One turret shot at `target`, with the fitted module's and its charge's effective attributes.
pub(super) async fn fire_turret(cx: &Ctx, fit: &Fit, module: &Module, target: i64) -> Result<(), CallError> {
    let ship_id = cx.ship_id;
    let Some((ship, t)) = cx.world.with(cx.system_id, move |s| Some((s.sampled_ball(ship_id)?, s.sampled_ball(target)?))).await.flatten() else { return Ok(()) };
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
pub(crate) struct Missile {
    pub(crate) charge: i32,
    pub(crate) raw: [f64; 4],
    pub(crate) aoe_cloud: f64,
    pub(crate) velocity: f64,
    pub(crate) flight_range: f64,
    pub(crate) weapon: i32,
    /// The pilot who fired it.
    pub(crate) attacker: Option<i64>,
}


/// Launch a native-steered missile actor; the separate gameplay impact timer
/// applies damage scaled by explosion radius versus the target's signature.
pub(crate) async fn launch_missile(db: Db, world: Arc<World>, system: i64, from: Ball, target: Ball, owner: i64, module: Option<i64>, m: Missile) -> Result<(), CallError> {
    let size = world.sde().types.get(m.charge)
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("unknown type {}", m.charge)))?;
    let mut slim = vec![("sourceShipID", Value::Int(from.id))];
    slim.push(("launchModules", Value::list(module.map(Value::Int))));
    let launch_ms = now_ms();
    let missile = Ball {
        id: NEXT_MISSILE_ID.fetch_add(1, Ordering::Relaxed),
        type_id: m.charge,
        owner_id: owner,
        corp_id: from.corp_id,
        alliance_id: -1,
        char_id: None,
        signature_radius: None,
        mode: crate::space::MODE_STOP,
        radius: size.radius.unwrap_or(1.0) as f32,
        mass: size.mass.unwrap_or(1.0),
        max_vel: m.velocity as f32,
        agility: 1.0,
        speed_fraction: 0.0,
        pos: from.pos,
        vel: [0.0; 3],
        old_pos: from.pos, old_vel: [0.0; 3], segment_acceleration: None,
        goto: target.pos,
        follow_id: 0,
        follow_range: 0.0,
        free: true,
        global: false,
        massive: false,
        interactive: false,
        warp: crate::space::WarpState::default(),
        warp_factor: 0,
        last_ms: launch_ms,
        interpolation_anchor_ms: launch_ms,
        effect_stamp: 0,
        cloak: 0,
        cloak_until: 0,
        health: [1.0; 3],
        launcher: 0,
        slim,
    };
    let missile_id = missile.id;
    let signature = attr(world.sde(), target.type_id, ATTR_SIGNATURE_RADIUS, target.radius as f64);
    let distance = dist(from.pos, target.pos);
    let delay_ms = seconds_to_ms((distance / m.velocity.max(1.0)).clamp(MIN_MISSILE_FLIGHT_SECONDS, MAX_MISSILE_FLIGHT_SECONDS));
    let (shooter_id, shooter_pilot) = (from.id, from.char_id);
    let (target_id, target_pilot) = (target.id, target.char_id);
    let (flight_range, aoe_cloud, raw, weapon, attacker) = (m.flight_range, m.aoe_cloud, m.raw, m.weapon, m.attacker);
    let world_ref = Arc::downgrade(&world);
    world.system(system).await.map_err(CallError::Internal)?;
    world.with(system, move |s| s.launch_missile(missile, target_id, shooter_id, delay_ms, move |s| {
        s.destroy_ball(missile_id);
        let present = s.contains(target_id);
        let reached = distance <= flight_range + MISSILE_REACH_ALLOWANCE_METRES;
        s.defer(async move {
            let Some(world) = world_ref.upgrade() else { return };
            let (dealt, quality) = if reached && present {
                let application = (signature / aoe_cloud.max(1.0)).min(1.0);
                (damage(&db, &world, system, target_id, target_pilot, raw.map(|d| d * application), attacker).await.unwrap_or(0.0), HIT_NORMAL)
            } else {
                (0.0, HIT_MISS)
            };
            report_hit(&world, shooter_id, target_id, shooter_pilot, target_pilot, weapon, dealt, quality);
        });
    })).await.ok_or_else(|| CallError::Internal(anyhow::anyhow!("system actor stopped")))?;
    Ok(())
}

pub(super) async fn fire_missile(cx: &Ctx, fit: &Fit, module: &Module, charge: i32, target: i64) -> Result<(), CallError> {
    let ship_id = cx.ship_id;
    let Some((ship, t)) = cx.world.with(cx.system_id, move |s| Some((s.sampled_ball(ship_id)?, s.sampled_ball(target)?))).await.flatten() else { return Ok(()) };
    let ch = Key::Charge(module.id);
    let velocity = fit.value(ch, ATTR_MAX_VELOCITY).max(1.0);
    let missile = Missile {
        charge,
        raw: ATTR_DAMAGE.map(|id| fit.value(ch, id) * fit.value(Key::Char, ATTR_MISSILE_DAMAGE_MULTIPLIER).max(0.0)),
        aoe_cloud: fit.value(ch, ATTR_AOE_CLOUD_SIZE),
        velocity,
        flight_range: velocity * ms_to_seconds(fit.value(ch, ATTR_EXPLOSION_DELAY)),
        weapon: module.type_id,
        attacker: Some(cx.char_id),
    };
    launch_missile(cx.db.clone(), cx.world.clone(), cx.system_id, ship, t, cx.char_id, Some(module.id), missile).await
}


#[cfg(test)] mod tests;
