//! Prepares, refreshes and saves drone shield, armor and hull condition.
//! No direct client mirror; combat and drone inventory services own the wire contract.
//! Uses fitted stats, shared Sde and game.item_attributes with game.items.
//! Capacitor and other drone attributes are not persisted here.

use super::*;
use crate::consts::OWNER_UNOWNED;
use crate::db::executor::Executor;

/// A prepared condition, not visible to combat until the inventory transition commits.
#[derive(Clone)]
pub(crate) struct HealthSeed(Hp);

impl HealthSeed {
    pub(crate) fn fractions(&self) -> [f64; 3] {
        [0, 1, 2].map(|i| if self.0.max[i] > 0.0 { (self.0.now[i] / self.0.max[i]).clamp(0.0, 1.0) } else { 0.0 })
    }
}

pub(crate) async fn prepare(db: &Db, fit: &Fit, key: Key, id: i64, persisted: bool) -> Result<HealthSeed, CallError> {
    let stats = ShipStats::from_fit(fit, key);
    let now = if persisted {
        let rows = crate::dao::items::dogma::overrides(db, id).await.map_err(db_err)?;
        let at = rows.iter().find(|r| r.attribute_id == ATTR_SHIELD_CHARGE).map(|r| r.updated_at).unwrap_or(0);
        let value = |attribute| rows.iter().find(|r| r.attribute_id == attribute).map(|r| r.value);
        restored_condition(&stats, [value(ATTR_SHIELD_CHARGE), value(ATTR_ARMOR_DAMAGE), value(ATTR_HULL_DAMAGE)], at, filetime_now()).0
    } else { stats.max };
    Ok(HealthSeed(Hp { now, max: stats.max, resonance: stats.resonance }))
}

pub(crate) fn install(id: i64, seed: HealthSeed) {
    state().hp.insert(id, seed.0);
}

pub(crate) async fn initialize(db: &Db, sde: Arc<Sde>, owner: i64, id: i64, type_id: i32) -> Result<[f64; 3], CallError> {
    let (fit, key) = if owner == OWNER_UNOWNED {
        (Fit::for_space_type(sde, type_id, Vec::new()), Key::Ship)
    } else {
        (Fit::drone(db, sde, owner, id).await?, Key::Drone(id))
    };
    let seed = prepare(db, &fit, key, id, true).await?;
    let mut st = state();
    let hp = st.hp.entry(id).or_insert(seed.0).clone();
    Ok(HealthSeed(hp).fractions())
}

fn rebase(hp: &mut Hp, stats: &ShipStats) {
    hp.now = [0, 1, 2].map(|i| (stats.max[i] - (hp.max[i] - hp.now[i]).max(0.0)).clamp(0.0, stats.max[i].max(0.0)));
    hp.max = stats.max;
    hp.resonance = stats.resonance;
}

pub(crate) async fn refresh(db: &Db, world: &World, system: i64, id: i64, owner: i64) -> Result<(), CallError> {
    let _lock = super::super::market::lock().await;
    refresh_locked(db, world, system, id, owner).await
}

pub(super) async fn refresh_locked(db: &Db, world: &World, system: i64, id: i64, owner: i64) -> Result<(), CallError> {
    let Some((record, ball)) = world.with(system, move |s| s.drone(id).zip(s.ball(id).cloned())).await.flatten() else { return Ok(()) };
    if record.owner != owner { return Ok(()); }
    let (fit, key, ship, ship_revision, drone_revision) = if owner == OWNER_UNOWNED {
        let Some((projections, revision)) = world.with(system, move |s| (s.projections_on(id), s.projection_revision(id))).await else { return Ok(()) };
        (Fit::for_space_type(world.sde().clone(), ball.type_id, projections), Key::Ship, None, None, Some(revision))
    } else {
        let ship = crate::dao::characters::selection::ship_id(db, owner).await.map_err(db_err)?
            .ok_or_else(|| CallError::Internal(anyhow::anyhow!("character has no selected ship")))?;
        let fit = Fit::drone_in_space(db, world, system, owner, id).await?;
        if crate::dao::characters::selection::ship_id(db, owner).await.map_err(db_err)? != Some(ship) {
            return Err(CallError::Internal(anyhow::anyhow!("drone owner ship changed during health refresh")));
        }
        let revisions = (fit.projection_revision(), fit.drone_projection_revision());
        (fit, Key::Drone(id), Some(ship), revisions.0, revisions.1)
    };
    let stats = ShipStats::from_fit(&fit, key);
    let seed = prepare(db, &fit, key, id, true).await?;
    let published = world.with(system, move |s| {
        if !s.drone(id).is_some_and(|d| d.owner == owner && d.generation == record.generation)
            || !s.ball(id).is_some_and(|b| b.type_id == ball.type_id)
            || drone_revision != Some(s.projection_revision(id))
            || ship.is_some_and(|ship| ship_revision != Some(s.projection_revision(ship))) { return false; }
        let mut st = state();
        let hp = st.hp.entry(id).or_insert(seed.0);
        if hp.now[2] <= 0.0 { return false; }
        rebase(hp, &stats);
        true
    }).await.unwrap_or(false);
    if !published { return Err(CallError::Internal(anyhow::anyhow!("drone health snapshot became stale"))); }
    Ok(())
}

pub(crate) async fn save_condition(db: &Db, id: i64) -> Result<(), CallError> {
    let _lock = super::super::market::lock().await;
    save_condition_in(db, id).await
}

pub(crate) async fn save_condition_in(ex: &(impl Executor + ?Sized), id: i64) -> Result<(), CallError> {
    let Some(hp) = state().hp.get(&id).cloned() else { return Ok(()) };
    crate::dao::items::dogma::save_condition(ex, id, &[ATTR_SHIELD_CHARGE, ATTR_ARMOR_DAMAGE, ATTR_HULL_DAMAGE],
        &[hp.now[0], hp.max[1] - hp.now[1], hp.max[2] - hp.now[2]], ATTR_SHIELD_CHARGE, filetime_now()).await.map_err(db_err)
}

#[cfg(test)] mod tests;
