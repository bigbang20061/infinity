//! NPC remote repairs using SDE dogma attributes and system-owned health.
//! Like player repair effects, range attenuates the repair amount. NPCs assist
//! only their spawn-group or owner allies, never their hostile target. Server
//! policy applies shield at cycle start and armor and hull at cycle end.
use super::{range::range_multiplier, registry::{Effect, EffectAttribute}, repair, space_source::{SourceContext, SourceGuard}, EffectKind};
use crate::consts::{dogma::EFFECT_NPC_BEHAVIOR_REMOTE_ARMOR, dogma::EFFECT_NPC_BEHAVIOR_REMOTE_SHIELD,
    dogma::EFFECT_NPC_REMOTE_ARMOR_REPAIRER, dogma::EFFECT_NPC_REMOTE_SHIELD_BOOSTER,
    dogma::EFFECT_NPC_REMOTE_HULL_REPAIRER};
use crate::services::{combat::groups_health::{npc_health_fraction, repair_npc_in_system, repair_player}, CallError};
use crate::space::{dist, Ball, System, MODE_WARP, drone::DronePhase};

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

pub(crate) async fn apply_source(cx: &SourceContext<'_>) -> Result<(), CallError> {
    let EffectKind::RemoteRepair { layer } = cx.effect.kind else { unreachable!("not a remote repair") };
    let optimal = cx.effect.attribute(EffectAttribute::Range).map_or(0.0, |attr| cx.fit.value(cx.key, attr));
    let falloff = cx.effect.attribute(EffectAttribute::Falloff).map_or(0.0, |attr| cx.fit.value(cx.key, attr));
    let base = cx.fit.value(cx.key, repair::amount_attribute(layer));
    let sde = cx.world.sde().clone();
    match cx.guard {
        SourceGuard::Npc { source, target: hostile, generation } => {
            if !matches!(cx.phase, DronePhase::Begin) || !supported_for_type(&sde, cx.source.type_id, cx.effect) { return Ok(()); }
            let duration_ms = cx.duration_ms;
            cx.world.with(cx.system, move |s| {
                if !s.npc_projection_valid(source, hostile, generation) { return; }
                let Some(target) = s.assist_target(source, |me, candidate| {
                    if candidate.id == hostile || me.mode == MODE_WARP || candidate.mode == MODE_WARP { return None; }
                    score(npc_health_fraction(candidate, layer), gap(me, candidate), optimal, falloff)
                }) else { return };
                let Some(me) = s.sampled_ball(source) else { return };
                let amount = repair::scaled_amount(base, gap(&me, &target), optimal, falloff);
                if repair::applies_at_start(layer) {
                    apply(s, &sde, source, hostile, target.id, generation, layer, amount, optimal, falloff);
                } else if duration_ms.is_finite() && duration_ms > 0.0 {
                    s.schedule(duration_ms as u64, move |s| apply(s, &sde, source, hostile, target.id, generation, layer, amount, optimal, falloff));
                }
            }).await;
        }
        SourceGuard::Drone { source, target, .. } => {
            if repair::applies_at_start(layer) != matches!(cx.phase, DronePhase::Begin) { return Ok(()); }
            if cx.effect.id == i64::from(crate::consts::dogma::EFFECT_MUTADAPTIVE_REMOTE_ARMOR_REPAIRER) {
                return Err(super::unsupported::refusal(Some(cx.effect.id as i32), "mutadaptive remote armor repair", cx.source.type_id));
            }
            let guard = cx.guard;
            let owner = cx.attacker;
            let valid = move |s: &System| {
                if !guard.valid(s) { return false; }
                let Some((me, other)) = s.sampled_ball(source).zip(s.sampled_ball(target)) else { return false };
                if me.mode == MODE_WARP || other.mode == MODE_WARP || range_multiplier(gap(&me, &other), optimal, falloff) <= 0.0 { return false; }
                if let Some(pilot) = other.char_id {
                    return owner.is_some_and(|owner| owner == pilot || crate::services::fleets::same_fleet(owner, pilot));
                }
                s.is_npc(target) || s.drone(target).is_some()
            };
            let Some((target_ball, distance, drone_owner)) = cx.world.with(cx.system, move |s| {
                if !valid(s) { return None; }
                let (me, other) = s.sampled_ball(source).zip(s.sampled_ball(target))?;
                let distance = gap(&me, &other);
                let drone_owner = s.drone(target).map(|drone| drone.owner);
                Some((other, distance, drone_owner))
            }).await.flatten() else { return Ok(()) };
            if let Some(owner) = drone_owner {
                crate::services::combat::drone_health::refresh(cx.db, cx.world, cx.system, target, owner).await?;
            }
            let amount = repair::scaled_amount(base, distance, optimal, falloff);
            if let Some(pilot) = target_ball.char_id {
                repair_player(cx.db, cx.world, cx.system, target, pilot, layer, amount, valid).await?;
            } else {
                cx.world.with(cx.system, move |s| {
                    if valid(s) { repair_npc_in_system(s, &sde, target, layer, amount); }
                }).await;
            }
        }
    }
    Ok(())
}

#[cfg(test)] mod tests;
