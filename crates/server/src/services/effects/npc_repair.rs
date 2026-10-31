//! NPC remote repairs using SDE dogma attributes and system-owned health.
//! Like player repair effects, range attenuates the repair amount. NPCs assist
//! only their spawn-group or owner allies, never their hostile target. Server
//! policy applies shield at cycle start and armor and hull at cycle end.
use super::{range::range_multiplier, registry::{Effect, EffectAttribute}, repair, EffectKind};
use crate::consts::{EFFECT_NPC_BEHAVIOR_REMOTE_ARMOR, EFFECT_NPC_BEHAVIOR_REMOTE_SHIELD,
    EFFECT_NPC_REMOTE_ARMOR_REPAIRER, EFFECT_NPC_REMOTE_SHIELD_BOOSTER,
    EFFECT_NPC_REMOTE_HULL_REPAIRER};
use crate::db::Db;
use crate::services::{combat::groups_health::{npc_health_fraction, repair_npc_in_system}, fit::{Fit, Key}, CallError};
use crate::space::{dist, Ball, System, World, MODE_WARP};

pub(crate) fn supported(id: i64) -> bool {
    [EFFECT_NPC_REMOTE_ARMOR_REPAIRER, EFFECT_NPC_REMOTE_SHIELD_BOOSTER,
        EFFECT_NPC_REMOTE_HULL_REPAIRER, EFFECT_NPC_BEHAVIOR_REMOTE_ARMOR, EFFECT_NPC_BEHAVIOR_REMOTE_SHIELD]
        .into_iter().any(|candidate| i64::from(candidate) == id)
}

pub(crate) fn supported_for_type(sde: &crate::sde::Sde, type_id: i32, effect: &Effect) -> bool {
    matches!(effect.kind, EffectKind::RemoteRepair { layer }
        if supported(effect.id) && sde.types.attribute(type_id, repair::amount_attribute(layer)).is_some())
}

fn gap(source: &Ball, target: &Ball) -> f64 {
    (dist(source.pos, target.pos) - f64::from(source.radius) - f64::from(target.radius)).max(0.0)
}

fn score(fraction: Option<f64>, distance: f64, optimal: f64, falloff: f64) -> Option<f64> {
    fraction.filter(|value| value.is_finite() && *value < 1.0 && range_multiplier(distance, optimal, falloff) > 0.0)
}

fn apply(s: &mut System, sde: &crate::sde::Sde, source: i64, hostile: i64, target: i64,
    generation: u64, layer: usize, amount: f64, optimal: f64, falloff: f64) {
    if !s.npc_projection_valid(source, hostile, generation) || target == hostile || !s.npc_assist_eligible(source, target) { return; }
    let Some((me, other)) = s.sampled_ball(source).zip(s.sampled_ball(target)) else { return };
    if me.mode == MODE_WARP || other.mode == MODE_WARP || range_multiplier(gap(&me, &other), optimal, falloff) <= 0.0 { return; }
    repair_npc_in_system(s, sde, target, layer, amount);
}

pub(crate) async fn apply_npc(
    _db: &Db, world: &World, system: i64, _source_module: i64, me: &Ball, victim: &Ball,
    effect: &Effect, duration_ms: f64, generation: u64,
) -> Result<(), CallError> {
    let EffectKind::RemoteRepair { layer } = effect.kind else { unreachable!("not an NPC remote repair") };
    if !supported_for_type(world.sde(), me.type_id, effect) { return Ok(()); }
    let source = me.id;
    let hostile = victim.id;
    let source_type = me.type_id;
    let sde = world.sde().clone();
    let effect = effect.clone();
    world.with(system, move |s| {
        if !s.npc_projection_valid(source, hostile, generation) { return; }
        let fit = Fit::for_space_type(sde.clone(), source_type, s.projections_on(source));
        let optimal = effect.attribute(EffectAttribute::Range).map_or(0.0, |attr| fit.value(Key::Ship, attr));
        let falloff = effect.attribute(EffectAttribute::Falloff).map_or(0.0, |attr| fit.value(Key::Ship, attr));
        let amount_attr = repair::amount_attribute(layer);
        let base = fit.value(Key::Ship, amount_attr);
        let Some(target) = s.assist_target(source, |me, candidate| {
            if candidate.id == hostile || me.mode == MODE_WARP || candidate.mode == MODE_WARP { return None; }
            score(npc_health_fraction(candidate, layer), gap(me, candidate), optimal, falloff)
        }) else { return };
        let Some(me) = s.sampled_ball(source) else { return };
        let amount = repair::scaled_amount(base, gap(&me, &target), optimal, falloff);
        if layer == 0 {
            apply(s, &sde, source, hostile, target.id, generation, layer, amount, optimal, falloff);
        } else if duration_ms.is_finite() && duration_ms > 0.0 {
            s.schedule(duration_ms as u64, move |s| apply(s, &sde, source, hostile, target.id, generation, layer, amount, optimal, falloff));
        }
    }).await;
    Ok(())
}

#[cfg(test)] mod tests;
