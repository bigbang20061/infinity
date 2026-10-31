//! Combat mechanics; state and routing live in the parent module.
use super::*;
use crate::services::effects::{notify::fire_fx, weapons::{hit_quality, launch_missile, report_hit, turret_roll, Missile}};
use crate::consts::ATTR_AGILITY;
use crate::sde::types::TypeData;
use crate::space::{MODE_STOP, V3};

/// Create the SDE-derived NPC ball and attach its existing combat brain.
pub(crate) async fn spawn(db: &Db, world: &Arc<World>, system: i64, data: &TypeData, corp: i64, pos: V3, tick: i64, owner: Option<i64>) -> Result<i64, CallError> {
    let ball = Ball {
        id: crate::services::missions::NEXT_NPC_ID.fetch_add(1, Ordering::Relaxed),
        type_id: data.type_id,
        owner_id: corp,
        corp_id: corp,
        alliance_id: -1,
        char_id: None,
        mode: MODE_STOP,
        radius: data.radius.unwrap_or(50.0) as f32,
        signature_radius: None,
        mass: data.mass.unwrap_or(1_000_000.0),
        max_vel: data.attribute(ATTR_MAX_VELOCITY).unwrap_or(300.0) as f32,
        agility: data.attribute(ATTR_AGILITY).unwrap_or(3.0) as f32,
        speed_fraction: 0.0,
        pos,
        vel: [0.0; 3],
        old_pos: pos,
        old_vel: [0.0; 3], segment_acceleration: None,
        goto: [0.0; 3],
        follow_id: 0,
        follow_range: 0.0,
        free: true,
        global: false,
        massive: true,
        interactive: false,
        warp: crate::space::WarpState::default(),
        warp_factor: 0,
        last_ms: tick,
        interpolation_anchor_ms: tick,
        effect_stamp: 0,
        cloak: 0,
        cloak_until: 0,
        health: [1.0; 3],
        launcher: 0,
        slim: vec![("corpID", Value::Int(corp))],
    };
    let id = ball.id;
    world.add_ship(system, ball).await.map_err(CallError::Internal)?;
    spawn_npc(db.clone(), world.clone(), system, id, data.type_id, owner).await?;
    Ok(id)
}

// ---------------------------------------------------------------- NPCs

/// An NPC's fighting attributes (entity attributes on its type).
pub(super) struct NpcStats {
    /// (shield boost, armor repair) amount per repair cycle, and the cycle (ms).
    pub(super) repairs: (f64, f64, f64),
    pub(super) scan_resolution: f64,
    pub(super) fly_range: f64,
    pub(super) aggro_range: f64,
    pub(super) rate_of_fire_ms: f64,
    pub(super) optimal: f64,
    pub(super) falloff: f64,
    pub(super) damage: [f64; DAMAGE_TYPE_COUNT],
    pub(super) missile: Option<(i32, f64)>,
    pub(super) fx: Option<String>,
}

pub(super) async fn npc_stats(db: &Db, sde: &Sde, type_id: i32) -> Result<NpcStats, CallError> {
    let multiplier = attr(sde, type_id, ATTR_DAMAGE_MULTIPLIER, 1.0);
    let fx = sde.types.effects(type_id).iter().filter_map(|entry| sde.dogma.effect(entry.effect_id))
        .find(|effect| effect.effect_category == Some(2)
            && effect.guid.as_deref().is_some_and(|guid| guid.starts_with("effects."))
            && effect.name.as_deref().is_some_and(|name| name != "missileLaunching"))
        .and_then(|effect| effect.guid.clone());
    let _ = db;
    let missile_type = attr(sde, type_id, ATTR_ENTITY_MISSILE_TYPE, 0.0) as i32;
    let missile = if missile_type > 0 {
        Some((missile_type, attr(sde, type_id, ATTR_MISSILE_DAMAGE_MULTIPLIER, 1.0)))
    } else {
        None
    };
    let optimal = attr(sde, type_id, ATTR_MAX_RANGE, 10_000.0);
    Ok(NpcStats {
        scan_resolution: attr(sde, type_id, ATTR_SCAN_RESOLUTION, 0.0),
        repairs: (
            attr(sde, type_id, ATTR_ENTITY_SHIELD_BOOST_AMOUNT, 0.0),
            attr(sde, type_id, ATTR_ENTITY_ARMOR_REPAIR_AMOUNT, 0.0),
            attr(sde, type_id, ATTR_ENTITY_ARMOR_REPAIR_DURATION, 0.0).max(attr(sde, type_id, ATTR_ENTITY_SHIELD_BOOST_DURATION, 0.0)).max(5_000.0),
        ),
        fly_range: attr(sde, type_id, ATTR_ENTITY_FLY_RANGE, 7_500.0).max(1_000.0),
        aggro_range: attr(sde, type_id, ATTR_ENTITY_ATTACK_RANGE, 0.0).max(optimal * 3.0).max(60_000.0),
        rate_of_fire_ms: attr(sde, type_id, ATTR_RATE_OF_FIRE, 4_000.0).max(1_000.0),
        optimal,
        falloff: attr(sde, type_id, ATTR_FALLOFF, 5_000.0),
        damage: ATTR_DAMAGE.map(|id| attr(sde, type_id, id, 0.0) * multiplier),
        missile,
        fx,
    })
}

/// Attach a brain to a spawned NPC; its effects run on the system's worker.


/// `me` and `victim` are the actor-sampled shot snapshots captured before worker dispatch.
pub(super) async fn npc_shot(db: &Db, world: &Arc<World>, system: i64, id: i64, type_id: i32, stats: &NpcStats, me: Ball, victim: Ball) -> Result<(), CallError> {
    let ship = victim.id;
    let gap = (dist(me.pos, victim.pos) - me.radius as f64 - victim.radius as f64).max(0.0);
    let owner_id = me.owner_id;
    let attacker_char_id = me.char_id;
    let victim_char_id = victim.char_id;
    if let Some((charge, multiplier)) = &stats.missile {
        let (charge, multiplier) = (*charge, *multiplier);
        let velocity = attr(world.sde(), charge, ATTR_MAX_VELOCITY, NPC_MISSILE_VELOCITY_MPS).max(1.0);
        let flight = velocity * ms_to_seconds(attr(world.sde(), charge, ATTR_EXPLOSION_DELAY, NPC_MISSILE_FLIGHT_MS));
        if gap <= flight {
            let missile = Missile {
                charge,
                raw: ATTR_DAMAGE.map(|a| attr(world.sde(), charge, a, 0.0) * multiplier),
                aoe_cloud: attr(world.sde(), charge, ATTR_AOE_CLOUD_SIZE, NPC_MISSILE_EXPLOSION_RADIUS_METRES),
                velocity,
                flight_range: flight,
                weapon: type_id,
                attacker: None,
            };
            launch_missile(db.clone(), world.clone(), system, me, victim, owner_id, None, missile).await?;
        }
    }
    if stats.damage.iter().any(|d| *d > 0.0) && gap <= stats.optimal + stats.falloff * NPC_TURRET_REACH_FALLOFF_MULTIPLIER {
        if let Some(guid) = stats.fx.as_deref() {
            let guid = guid.to_owned();
            let duration = stats.rate_of_fire_ms;
            world.broadcast(system, id, move |stamp| vec![fire_fx(stamp, id, None, &guid, Some(ship), None, true, true, duration, 1)]);
        }
        let multiplier = turret_roll(gap, stats.optimal, stats.falloff);
        let dealt = if multiplier > 0.0 { damage(db, world, system, ship, victim_char_id, stats.damage.map(|d| d * multiplier), None).await? } else { 0.0 };
        report_hit(world, id, ship, attacker_char_id, victim_char_id, type_id, dealt, hit_quality(multiplier));
    }
    Ok(())
}




/// A damaged NPC boosts its shield and repairs its armor every repair cycle.
pub(super) fn npc_repair(world: &World, system: i64, id: i64, stats: &NpcStats) {
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


