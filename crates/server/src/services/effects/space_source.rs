//! Shared dispatch for actor-owned in-space effect sources.
use std::sync::Arc;
use crate::db::Db;
use crate::services::{CallError, fit::{Fit, Key}};
use crate::space::{Ball, System, World, drone::{DroneOrder, DronePhase, DroneRecord}};
use super::{EffectKind, registry::Effect};

#[derive(Clone, Copy)]
pub(crate) enum SourceGuard {
    Npc { source: i64, target: i64, generation: u64 },
    Drone { source: i64, target: i64, generation: u64 },
}

impl SourceGuard {
    pub(crate) fn valid(self, s: &System) -> bool {
        match self {
            Self::Npc { source, target, generation } => s.npc_projection_valid(source, target, generation),
            Self::Drone { source, target, generation } => {
                if !s.drone_valid(source, target, generation) { return false; }
                let Some(drone) = s.drone(source) else { return false };
                match drone.order {
                    DroneOrder::Assist(delegate) | DroneOrder::Guard(delegate) =>
                        crate::services::fleets::same_fleet(drone.owner, delegate)
                        && s.pilot_ball(delegate).is_some_and(|ball| {
                            let Some(sde) = s.sde() else { return false };
                            let capsule_group = sde.types.get(crate::consts::TYPE_CAPSULE).and_then(|ty| ty.group_id);
                            sde.types.get(ball.type_id).is_some_and(|ty|
                                ty.category_id == Some(crate::consts::CATEGORY_SHIP)
                                && ty.group_id.is_some() && capsule_group.is_some() && ty.group_id != capsule_group)
                                && ball.mode != crate::space::MODE_WARP && ball.cloak == 0
                        }),
                    _ => true,
                }
            }
        }
    }
}

pub(crate) struct SourceContext<'a> {
    pub db: &'a Db,
    pub world: &'a Arc<World>,
    pub system: i64,
    pub source: &'a Ball,
    pub target: &'a Ball,
    pub module: i64,
    pub effect: &'a Effect,
    pub duration_ms: f64,
    pub fit: &'a Fit,
    pub key: Key,
    pub guard: SourceGuard,
    pub attacker: Option<i64>,
    pub drone: Option<&'a DroneRecord>,
    pub phase: DronePhase,
}

pub(crate) async fn apply(cx: &SourceContext<'_>) -> Result<(), CallError> {
    match cx.effect.kind {
        EffectKind::Turret | EffectKind::Missile => super::weapons::fire_source(cx).await,
        EffectKind::Projected { .. } => super::projected::apply_source(cx).await,
        EffectKind::Ecm => super::ecm::apply_source(cx).await,
        EffectKind::Neutralizer | EffectKind::Nosferatu | EffectKind::CapTransfer => super::npc_capacitor::apply_source(cx).await,
        EffectKind::RemoteRepair { .. } => super::npc_repair::apply_source(cx).await,
        EffectKind::Mining if matches!(cx.phase, DronePhase::End) && cx.drone.is_some() =>
            crate::services::drones::mining::finish_cycle(cx.db, cx.world, cx.system, cx.drone.unwrap().clone(), cx.target.id, cx.fit).await,
        EffectKind::Mining if matches!(cx.phase, DronePhase::Begin) && cx.drone.is_some() => Ok(()),
        EffectKind::Smartbomb if matches!(cx.guard, SourceGuard::Npc { .. }) => {
            let SourceGuard::Npc { generation, .. } = cx.guard else { unreachable!() };
            super::aoe::apply_npc(cx.db, cx.world, cx.system, cx.module, cx.source, cx.target, cx.effect, cx.duration_ms, generation).await
        }
        _ => {
            let id = cx.effect.id as i32;
            let name = cx.world.sde().dogma.effect(id).and_then(|row| row.name.as_deref()).unwrap_or("unknown");
            Err(super::unsupported::refusal(Some(id), name, cx.source.type_id))
        }
    }
}

#[cfg(test)] mod tests;
