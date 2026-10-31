//! NPC-source neutralizers, nosferatu and capacitor transfers.
//! Like player capacitor effects, these use SDE dogma effect range metadata and
//! combat capacitor state; NPC source attributes come from the entity type's SDE.
//! NPCs have no fitted-module capacitor cost; transfers assist eligible NPCs
//! without Crimewatch flags, and do nothing if no recipient needs energy.
//! If a target is lost between nosferatu's drain and injection, the transfer
//! ends with that target generation; transient NPC capacitor is not persisted.
use super::capacitor::nos_allowed;
use super::range::range_multiplier;
use super::registry::{Effect, EffectAttribute};
use super::EffectKind;
use crate::consts::{ATTR_ENERGY_NEUTRALIZER_AMOUNT, ATTR_NOS_OVERRIDE, ATTR_POWER_TRANSFER_AMOUNT};
use crate::db::Db;
use crate::services::combat::{attr, groups_health::{capacitor_snapshot, drain_exact_guarded, inject_guarded, npc_capacitor_fraction, NpcCapacitorGuard}};
use crate::services::CallError;
use crate::space::{dist, Ball, World};

fn scaled_amount(amount: f64, distance: f64, optimal: f64, falloff: f64) -> f64 {
    if !amount.is_finite() { return 0.0; }
    amount.max(0.0) * range_multiplier(distance, optimal, falloff)
}
pub(crate) fn in_range(distance: f64, optimal: f64, falloff: f64) -> bool {
    range_multiplier(distance, optimal, falloff) > 0.0
}


pub(crate) async fn apply_npc(
    db: &Db, world: &World, system: i64, _source_module: i64, me: &Ball, victim: &Ball,
    effect: &Effect, _duration_ms: f64, generation: u64,
) -> Result<(), CallError> {
    let sde = world.sde();
    let value = |id| attr(sde, me.type_id, id, 0.0);
    let optimal = effect.attribute(EffectAttribute::Range).map(value).unwrap_or(0.0);
    let falloff = effect.attribute(EffectAttribute::Falloff).map(value).unwrap_or(0.0);
    let distance = (dist(me.pos, victim.pos) - f64::from(me.radius) - f64::from(victim.radius)).max(0.0);
    let guard = NpcCapacitorGuard::Hostile { source: me.id, target: victim.id, generation };
    match effect.kind {
        EffectKind::Neutralizer => {
            let amount = scaled_amount(value(ATTR_ENERGY_NEUTRALIZER_AMOUNT), distance, optimal, falloff);
            drain_exact_guarded(db, world, system, victim.id, amount, guard).await?;
        }
        EffectKind::Nosferatu => {
            let Some(source) = capacitor_snapshot(db, world, system, me.id).await? else { return Ok(()) };
            let Some(target) = capacitor_snapshot(db, world, system, victim.id).await? else { return Ok(()) };
            if nos_allowed(source.level, source.capacity, target.level, target.capacity, value(ATTR_NOS_OVERRIDE) > 0.0) {
                let amount = scaled_amount(value(ATTR_POWER_TRANSFER_AMOUNT), distance, optimal, falloff);
                let drained = drain_exact_guarded(db, world, system, victim.id, amount, guard).await?;
                inject_guarded(db, world, system, me.id, drained, guard).await?;
            }
        }
        EffectKind::CapTransfer => {
            let source = me.id;
            let hostile = victim.id;
            let sde = sde.clone();
            let Some((source_ball, target)) = world.with(system, move |s| {
                if !s.npc_projection_valid(source, hostile, generation) { return None; }
                let target = s.assist_target(source, |me, candidate| {
                    let distance = (dist(me.pos, candidate.pos) - f64::from(me.radius) - f64::from(candidate.radius)).max(0.0);
                    if !in_range(distance, optimal, falloff) { return None; }
                    npc_capacitor_fraction(&sde, candidate).filter(|fraction| *fraction < 1.0)
                })?;
                Some((s.sampled_ball(source)?, target))
            }).await.flatten() else { return Ok(()) };
            let distance = (dist(source_ball.pos, target.pos) - f64::from(source_ball.radius) - f64::from(target.radius)).max(0.0);
            let amount = scaled_amount(value(ATTR_POWER_TRANSFER_AMOUNT), distance, optimal, falloff);
            let guard = NpcCapacitorGuard::Assistance { source, hostile, target: target.id, generation, optimal, falloff };
            inject_guarded(db, world, system, target.id, amount, guard).await?;
        }
        _ => unreachable!("not an NPC capacitor effect"),
    }
    Ok(())
}

#[cfg(test)] mod tests;
