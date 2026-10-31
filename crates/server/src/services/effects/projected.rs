//! Snapshot and apply targeted dogma modifiers for ships and NPCs.
//! Mirrors the projected effects in dogma/pythonEffects and uses SDE modifier
//! rows with effective source Fit values. Lifetime and notifications belong to
//! the system actor; effects with no target modifier rows are not synthesized here.
use super::{projection::Projection, range::range_multiplier, registry::{Effect, EffectAttribute}};
use crate::db::Db;
use crate::messages;
use crate::sde::{dogma::Modifier, Sde};
use crate::services::{combat::Ctx, fit::{Fit, Key, OP_PRE_MUL, OP_PRE_DIV, OP_POST_MUL, OP_POST_DIV}, user_error, CallError};
use crate::space::{dist, seconds_to_ms, Ball, World};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Scale multiplier and divisor operands around their neutral value, rather
/// than scaling the operand itself. Percent and additive operands have zero
/// as their neutral value.
fn scaled(value: f64, op: i32, strength: f64) -> f64 {
    match op {
        OP_PRE_MUL | OP_PRE_DIV | OP_POST_MUL | OP_POST_DIV => 1.0 + (value - 1.0) * strength,
        _ => value * strength,
    }
}

fn snapshot_rows(fit: &Fit, key: Key, rows: &[Modifier], values: &mut Vec<(i32, f64, i32)>) {
    for row in rows.iter().filter(|row| row.domain == "targetID") {
        if values.iter().any(|entry| entry.0 == row.modifying) { continue; }
        let source = crate::sde::dogma::python_effects::projected_interim_source(row.modifying).unwrap_or(row.modifying);
        values.push((row.modifying, fit.value(key, source), row.op));
    }
}

fn snapshot(sde: &Sde, fit: &Fit, key: Key, effect: &Effect, charge_type: Option<i32>) -> (Vec<(i32, f64, i32)>, Vec<(i32, f64, i32)>) {
    let mut values = Vec::new();
    let mut charge_values = Vec::new();
    if let Some(row) = sde.dogma.effect(effect.id as i32) {
        snapshot_rows(fit, key, row.modifiers(), &mut values);
    }
    if let (Key::Module(module), Some(charge)) = (key, charge_type) {
        for entry in sde.types.effects(charge) {
            if let Some(row) = sde.dogma.effect(entry.effect_id) {
                snapshot_rows(fit, Key::Charge(module), row.modifiers(), &mut charge_values);
            }
        }
    }
    (values, charge_values)
}

fn missing_target() -> CallError {
    user_error(messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT, vec![])
}

#[allow(clippy::too_many_arguments)]
async fn insert(world: &Arc<World>, system: i64, source: i64, target: i64, module: i64, source_type: i32, effect: &Effect, duration_ms: f64, charge_type: Option<i32>, snapshots: (Vec<(i32, f64, i32)>, Vec<(i32, f64, i32)>), optimal: f64, falloff: f64, npc_generation: Option<u64>, player: Option<(i64, Arc<AtomicBool>)>) -> Result<(), CallError> {
    let kind = effect.kind;
    let effect_id = effect.id as i32;
    let inserted = world.with(system, move |s| {
        if let Some((character, generation)) = &player {
            if generation.load(Ordering::Relaxed) || !crate::services::combat::targeting_charges::targets_of(*character).contains(&target) { return false; }
            let state = crate::services::combat::state();
            if !state.active.get(&module).is_some_and(|active| Arc::ptr_eq(&active.stop, generation)) { return false; }
        }
        if npc_generation.is_some_and(|generation| !s.npc_projection_valid(source, target, generation)) { return false; }
        let Some((a, b)) = s.sampled_ball(source).zip(s.sampled_ball(target)) else { return false };
        let gap = (dist(a.pos, b.pos) - f64::from(a.radius) - f64::from(b.radius)).max(0.0);
        let strength = range_multiplier(gap, optimal, falloff);
        if strength <= 0.0 { return false; }
        let scale = |entries: Vec<(i32, f64, i32)>| entries.into_iter().map(|(attr, value, op)| (attr, scaled(value, op, strength))).collect();
        let values = scale(snapshots.0);
        let charge_values = scale(snapshots.1);
        let until_ms = if duration_ms < 0.0 { i64::MAX } else {
            s.now_ms().saturating_add(duration_ms as i64).saturating_add(seconds_to_ms(1.0) as i64)
        };
        s.upsert_projection(target, Projection { source_ship: source, source_module: module, source_type, effect_id, kind, until_ms, values, charge_values, charge_type });
        true
    }).await.unwrap_or(false);
    if inserted { Ok(()) } else { Err(missing_target()) }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn apply(cx: &Ctx, fit: &Fit, source_module: i64, source_type: i32, effect: &Effect, target: i64, duration_ms: f64, charge_type: Option<i32>) -> Result<(), CallError> {
    let key = Key::Module(source_module);
    let generation = crate::services::combat::state().active.get(&source_module).map(|active| active.stop.clone()).ok_or_else(missing_target)?;
    let optimal = effect.attribute(EffectAttribute::Range).map_or(0.0, |attr| fit.value(key, attr));
    let falloff = effect.attribute(EffectAttribute::Falloff).map_or(0.0, |attr| fit.value(key, attr));
    let values = snapshot(cx.world.sde(), fit, key, effect, charge_type);
    insert(&cx.world, cx.system_id, cx.ship_id, target, source_module, source_type, effect, duration_ms, charge_type, values, optimal, falloff, None, Some((cx.char_id, generation))).await?;
    if matches!(effect.kind, super::EffectKind::Projected { jamming: super::JammingType::Scramble }) {
        super::lifecycle::stop_blocked_propulsion(&cx.db, &cx.world, cx.system_id, target).await;
    }
    Ok(())
}

/// NPC module identities are allocated by the caller and remain stable across cycles.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn apply_npc(db: &Db, world: &Arc<World>, system: i64, source_module: i64, source: &Ball, target: &Ball, effect: &Effect, duration_ms: f64, generation: u64) -> Result<(), CallError> {
    let source_id = source.id;
    let projections = world.with(system, move |s| s.projections_on(source_id)).await.ok_or_else(missing_target)?;
    let fit = Fit::for_space_type(world.sde().clone(), source.type_id, projections);
    let key = Key::Ship;
    let optimal = effect.attribute(EffectAttribute::Range).map_or(0.0, |attr| fit.value(key, attr));
    let falloff = effect.attribute(EffectAttribute::Falloff).map_or(0.0, |attr| fit.value(key, attr));
    let values = snapshot(world.sde(), &fit, key, effect, None);
    insert(world, system, source.id, target.id, source_module, source.type_id, effect, duration_ms, None, values, optimal, falloff, Some(generation), None).await?;
    if matches!(effect.kind, super::EffectKind::Projected { jamming: super::JammingType::Scramble }) {
        super::lifecycle::stop_blocked_propulsion(db, world, system, target.id).await;
    }
    Ok(())
}

#[cfg(test)] mod tests;
