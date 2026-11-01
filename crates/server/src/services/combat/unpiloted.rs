//! Durable health and destruction of persistent unpiloted ship balls.
//! Uses fitted ship condition from game.item_attributes and the system's persistent identity.
//! Client damage state is published after commit; free hull losses do not create rewards or wrecks.
use super::*;
use crate::consts::CATEGORY_SHIP;

async fn delete_in(ex: &(impl Executor + ?Sized), ship: i64) -> Result<(), CallError> {
    for reload in crate::dao::items::pending_reloads::for_ship(ex, ship).await.map_err(db_err)? {
        crate::dao::items::pending_reloads::take(ex, reload.reload_id).await.map_err(db_err)?;
    }
    crate::dao::insurance::take_payout(ex, ship).await.map_err(db_err)?;
    crate::dao::items::ship::delete_lost(ex, ship).await.map_err(db_err)
}
fn rebase(cond: &mut Condition, stats: ShipStats) {
    cond.now[0] = cond.now[0].min(stats.max[0]);
    for i in 1..3 {
        let damage = (cond.stats.max[i] - cond.now[i]).max(0.0);
        cond.now[i] = (stats.max[i] - damage).clamp(0.0, stats.max[i].max(0.0));
    }
    cond.stats = stats;
}


/// Execute only for a currently registered, unpiloted ship; `None` means another ball kind.
pub(crate) async fn damage(db: &Db, world: &World, system: i64, ship: i64, raw: [f64; 4]) -> Result<Option<f64>, CallError> {
    let sde = world.sde().clone();
    let Some((generation, type_id, owner)) = world.with(system, move |s| {
        let ball = s.ball(ship)?;
        let generation = s.persistent_generation(ship)?;
        (ball.char_id.is_none() && sde.types.get(ball.type_id).and_then(|t| t.category_id) == Some(CATEGORY_SHIP))
            .then_some((generation, ball.type_id, ball.owner_id))
    }).await.flatten() else { return Ok(None) };
    let _lock = super::super::market::lock().await;
    if !world.with(system, move |s| s.persistent_generation(ship) == Some(generation)
        && s.ball(ship).is_some_and(|b| b.char_id.is_none() && b.type_id == type_id && b.owner_id == owner)).await.unwrap_or(false) {
        return Ok(Some(0.0));
    }
    let fit = Fit::load_unpiloted_in_space(db, world, system, ship).await?;
    let revision = fit.projection_revision();
    let stats = ShipStats::from_fit(&fit, Key::Ship);
    let known = state().condition.get(&ship).cloned();
    let mut cond = match known {
        Some(cond) => cond,
        None => prepare_condition_with_fit(db, ship, &fit).await?.0,
    };
    rebase(&mut cond, stats);
    let now = filetime_now();
    cond.now[0] = recharge(cond.now[0], cond.stats.max[0], cond.stats.shield_tau_ms, now.saturating_sub(cond.shield_at) as f64 / FILETIME_SECOND as f64);
    cond.shield_at = now;
    let dealt = absorb(&mut cond.now, &cond.stats.resonance, raw);
    let lethal = cond.now[2] <= 0.0;
    let valid = world.with(system, move |s| s.persistent_generation(ship) == Some(generation)
        && revision == Some(s.projection_revision(ship))
        && s.ball(ship).is_some_and(|b| b.char_id.is_none() && b.type_id == type_id && b.owner_id == owner)).await.unwrap_or(false);
    if !valid { return Ok(Some(0.0)); }
    let committed = db.transaction(|tx, _: &mut Outbox| {
        let cond = cond.clone();
        Box::pin(async move {
            if !crate::dao::items::space_ships::claim_unpiloted(tx, ship, type_id, owner, system).await.map_err(db_err)? { return Ok(false); }
            if lethal { delete_in(tx, ship).await?; }
            else { persist_condition_in(tx, ship, &cond).await?; }
            Ok::<bool, CallError>(true)
        })
    }, |value, _| async move { Ok(value) }).await?;
    if !committed { return Ok(Some(0.0)); }
    if lethal {
        let destroyed = world.with(system, move |s| {
            if s.persistent_generation(ship) == Some(generation)
                && s.ball(ship).is_some_and(|b| b.char_id.is_none() && b.type_id == type_id && b.owner_id == owner) {
                s.destroy_ball(ship);
                forget_ship(ship);
                true
            } else { false }
        }).await.unwrap_or(false);
        if destroyed { forget_target(world, ship); }
    } else {
        let health = [0, 1, 2].map(|i| if cond.stats.max[i] > 0.0 { (cond.now[i] / cond.stats.max[i]).clamp(0.0, 1.0) } else { 0.0 });
        world.with(system, move |s| {
            if s.persistent_generation(ship) == Some(generation)
                && s.ball(ship).is_some_and(|b| b.char_id.is_none() && b.type_id == type_id && b.owner_id == owner) {
                state().condition.insert(ship, cond);
                s.set_health(ship, health);
            }
        }).await;
    }
    Ok(Some(dealt))
}

/// Repair a free hull durably, or use the existing actor-owned NPC repair path.
pub(crate) async fn repair(db: &Db, world: &World, system: i64, ship: i64, layer: usize, amount: f64,
    guard: impl Fn(&crate::space::System) -> bool + Clone + Send + 'static) -> Result<(), CallError> {
    if layer >= 3 || !amount.is_finite() || amount <= 0.0 { return Ok(()); }
    let sde = world.sde().clone();
    let initial_guard = guard.clone();
    let Some((generation, type_id, owner)) = world.with(system, move |s| {
        if !initial_guard(s) { return None; }
        let ball = s.ball(ship)?;
        let category = sde.types.get(ball.type_id).and_then(|t| t.category_id);
        if ball.char_id.is_none() && category == Some(CATEGORY_SHIP) {
            if let Some(generation) = s.persistent_generation(ship) {
                return Some((generation, ball.type_id, ball.owner_id));
            }
        }
        groups_health::repair_npc_in_system(s, &sde, ship, layer, amount);
        None
    }).await.flatten() else { return Ok(()) };
    let _lock = super::super::market::lock().await;
    if !world.with(system, move |s| s.persistent_generation(ship) == Some(generation)
        && s.ball(ship).is_some_and(|b| b.char_id.is_none() && b.type_id == type_id && b.owner_id == owner)).await.unwrap_or(false) {
        return Ok(());
    }
    let fit = Fit::load_unpiloted_in_space(db, world, system, ship).await?;
    let revision = fit.projection_revision();
    let stats = ShipStats::from_fit(&fit, Key::Ship);
    let known = state().condition.get(&ship).cloned();
    let mut cond = match known {
        Some(cond) => cond,
        None => prepare_condition_with_fit(db, ship, &fit).await?.0,
    };
    if cond.now[2] <= 0.0 { return Ok(()); }
    rebase(&mut cond, stats);
    let now = filetime_now();
    cond.now[0] = recharge(cond.now[0], cond.stats.max[0], cond.stats.shield_tau_ms, now.saturating_sub(cond.shield_at) as f64 / FILETIME_SECOND as f64);
    cond.shield_at = now;
    cond.now[layer] = (cond.now[layer] + amount).min(cond.stats.max[layer]);
    let fit_guard = guard.clone();
    let valid = world.with(system, move |s| fit_guard(s) && s.persistent_generation(ship) == Some(generation)
        && revision == Some(s.projection_revision(ship))
        && s.ball(ship).is_some_and(|b| b.char_id.is_none() && b.type_id == type_id && b.owner_id == owner)).await.unwrap_or(false);
    if !valid { return Ok(()); }
    let committed = db.transaction(|tx, _: &mut Outbox| {
        let cond = cond.clone();
        Box::pin(async move {
            if !crate::dao::items::space_ships::claim_unpiloted(tx, ship, type_id, owner, system).await.map_err(db_err)? { return Ok(false); }
            persist_condition_in(tx, ship, &cond).await?;
            Ok::<bool, CallError>(true)
        })
    }, |value, _| async move { Ok(value) }).await?;
    if committed {
        let health = [0, 1, 2].map(|i| if cond.stats.max[i] > 0.0 { (cond.now[i] / cond.stats.max[i]).clamp(0.0, 1.0) } else { 0.0 });
        world.with(system, move |s| {
            if s.persistent_generation(ship) == Some(generation)
                && s.ball(ship).is_some_and(|b| b.char_id.is_none() && b.type_id == type_id && b.owner_id == owner) {
                state().condition.insert(ship, cond);
                s.set_health(ship, health);
            }
        }).await;
    }
    Ok(())
}

#[cfg(test)] mod tests;
