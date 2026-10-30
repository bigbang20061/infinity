//! NPC standings and loyalty points (standingMgr, LPSvc, LPStoreMgr;
//! eve/client/script/ui/services/standingsvc.py, station/loyaltyPointStore).
//!
//! Standings (game.standings) are what NPC agents, corporations and factions think of a
//! character, from -10 to +10. Completing an agent's mission raises the agent's, its
//! corporation's and its faction's; declining or quitting one costs a little with the agent
//! and its corporation. Gains and losses move a standing part of the way to +10 or -10, so
//! they shrink as it approaches the limit. Agents above level 1 need an effective standing
//! (the best of the three, unless any is -2 or worse) of 1, 3, 5 or 7.
//!
//! Loyalty points (game.lp_balances) are paid per corporation by missions and spent in that
//! corporation's LP store. The live stores' offer tables never shipped with the client or
//! the SDE, so a store here sells the faction items (meta group 4) of the corporation's race
//! that traded on the retail market, at one LP per 2,000 ISK of retail value plus 1,000 ISK
//! per LP.

use super::{db_err, notify_error, Call, CallError, CallResult};
use crate::client::Client;
use crate::db::Db;
use crate::db::executor::Executor;
use crate::db::outbox::Outbox;
use crate::messages;
use crate::space::World;
use marshal::Value;

mod standing_mgr;
mod lp_svc;
mod lp_store_mgr;

/// Server mission policy: corporation gain per agent level.
const MISSION_CORP_GAIN: f64 = 0.01;
/// Server mission policy: decline or quit standing loss.
const DECLINE_LOSS: f64 = -0.01;
/// Client standings convention: absolute standing limit.
const STANDING_LIMIT: f64 = 10.0;
/// Client standingsvc.GetEffectiveStandingWithAgent rejection threshold.
const AGENT_STANDING_REJECTION: f64 = -2.0;
/// Server mission policy: agent gain multiplier.
const MISSION_AGENT_GAIN_MULTIPLIER: f64 = 2.0;
/// Server mission policy: faction gain divisor.
const MISSION_FACTION_GAIN_DIVISOR: f64 = 5.0;
/// Client standingsvc agent access: required standing for levels 2 through 5.
const AGENT_LEVEL_STANDINGS: [f64; 4] = [1.0, 3.0, 5.0, 7.0];
/// Client standingsvc agent access: level two.
const AGENT_LEVEL_TWO: i64 = 2;
/// Client standingsvc agent access: level three.
const AGENT_LEVEL_THREE: i64 = 3;
/// Client standingsvc agent access: level four.
const AGENT_LEVEL_FOUR: i64 = 4;

pub async fn standing(db: &Db, char_id: i64, from: i64) -> Result<f64, CallError> {
    Ok(crate::dao::characters::standings::standing(db, char_id, from).await.map_err(db_err)?.unwrap_or(0.0))
}

/// Move `from`'s standing toward the character `change` of the way to +10 (or -10 if negative).
pub async fn modify(db: &Db, world: &World, char_id: i64, from: i64, change: f64) -> Result<(), CallError> {
    if from == 0 || change == 0.0 {
        return Ok(());
    }
    let old = standing(db, char_id, from).await?;
    let new = if change > 0.0 { old + (STANDING_LIMIT - old) * change } else { old + (STANDING_LIMIT + old) * change }.clamp(-STANDING_LIMIT, STANDING_LIMIT);
    crate::dao::characters::standings::set(db, char_id, from, new).await.map_err(db_err)?;
    world.notify_pilot(char_id, "OnStandingSet", vec![Value::Int(from), Value::Int(char_id), Value::Float(new)]);
    Ok(())
}

/// standingsvc.GetEffectiveStandingWithAgent: the best of the agent's, its corporation's and
/// its faction's standing, unless one of them is -2 or worse.
pub async fn effective(db: &Db, char_id: i64, agent: i64, corp: i64, faction: i64) -> Result<f64, CallError> {
    let s = [standing(db, char_id, faction).await?, standing(db, char_id, corp).await?, standing(db, char_id, agent).await?];
    let min = s.iter().copied().fold(f64::INFINITY, f64::min);
    Ok(if min <= AGENT_STANDING_REJECTION { min } else { s.iter().copied().fold(f64::NEG_INFINITY, f64::max) })
}

/// The effective standing an agent of `level` asks for.
pub fn required_for_level(level: i64) -> f64 {
    match level {
        ..=1 => f64::NEG_INFINITY,
        AGENT_LEVEL_TWO => AGENT_LEVEL_STANDINGS[0],
        AGENT_LEVEL_THREE => AGENT_LEVEL_STANDINGS[1],
        AGENT_LEVEL_FOUR => AGENT_LEVEL_STANDINGS[2],
        _ => AGENT_LEVEL_STANDINGS[3],
    }
}

/// A mission was completed for an agent of `level`.
pub async fn mission_completed(db: &Db, world: &World, char_id: i64, agent: i64, corp: i64, faction: i64, level: i64) -> Result<(), CallError> {
    let gain = MISSION_CORP_GAIN * level.max(1) as f64;
    modify(db, world, char_id, agent, gain * MISSION_AGENT_GAIN_MULTIPLIER).await?;
    modify(db, world, char_id, corp, gain).await?;
    modify(db, world, char_id, faction, gain / MISSION_FACTION_GAIN_DIVISOR).await
}

/// A mission was declined or quit.
pub async fn mission_declined(db: &Db, world: &World, char_id: i64, agent: i64, corp: i64) -> Result<(), CallError> {
    modify(db, world, char_id, agent, DECLINE_LOSS).await?;
    modify(db, world, char_id, corp, DECLINE_LOSS).await
}

// ---------------------------------------------------------------- loyalty points

pub async fn lp_balance(db: &(impl Executor + ?Sized), char_id: i64, corp: i64) -> Result<i64, CallError> {
    Ok(crate::dao::wallets::loyalty::balance(db, char_id, corp).await.map_err(db_err)?.unwrap_or(0))
}

/// Add (or, negative, take) loyalty points with `corp`; taking more than there is fails.
pub async fn add_lp<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, char_id: i64, corp: i64, amount: i64) -> Result<(), CallError> {
    if amount == 0 || corp == 0 {
        return Ok(());
    }
    let before = lp_balance(db, char_id, corp).await?;
    if before + amount < 0 {
        return Err(notify_error(&messages::loyalty_balance_message(before)));
    }
    crate::dao::wallets::loyalty::add(db, char_id, corp, amount).await.map_err(db_err)?;
    outbox.notify_pilot(char_id, "OnLPChange", vec![Value::str("lp"), Value::Int(corp), Value::Int(before), Value::Int(before + amount)]);
    Ok(())
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.service {
        "standingMgr" => standing_mgr::dispatch(c, call).await,
        "LPSvc" => lp_svc::dispatch(c, call).await,
        "LPStoreMgr" => lp_store_mgr::dispatch(c, call).await,
        _ => None,
    }
}
