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

use super::{db_err, keyval, notify_error, rowset, Call, CallError, CallResult};
use crate::client::Client;
use crate::db::Db;
use crate::db::executor::Executor;
use crate::db::outbox::{dispatch_world, Outbox};
use crate::messages;
use crate::space::World;
use marshal::Value;
use tracing::info;

/// Server mission policy: corporation gain per agent level.
const MISSION_CORP_GAIN: f64 = 0.01;
/// Server mission policy: decline or quit standing loss.
const DECLINE_LOSS: f64 = -0.01;
/// Server LP store policy: retail ISK value per LP.
const ISK_VALUE_PER_LP: f64 = 2000.0;
/// Server LP store policy: ISK charged per LP.
const ISK_PER_LP: i64 = 1000;
/// SDE metaGroups: faction.
const META_GROUP_FACTION: f64 = 4.0;
/// Client standings convention: absolute standing limit.
const STANDING_LIMIT: f64 = 10.0;
/// Client standingsvc.GetEffectiveStandingWithAgent rejection threshold.
const AGENT_STANDING_REJECTION: f64 = -2.0;
/// Server mission policy: agent gain multiplier.
const MISSION_AGENT_GAIN_MULTIPLIER: f64 = 2.0;
/// Server mission policy: faction gain divisor.
const MISSION_FACTION_GAIN_DIVISOR: f64 = 5.0;
/// Server LP store policy: offer prices are rounded to this LP increment.
const LP_PRICE_INCREMENT: i64 = 25;
/// Server LP store policy: minimum LP price.
const MIN_LP_PRICE: i64 = 100;
/// Server LP store policy: maximum quantity per purchase.
const MAX_OFFER_COUNT: i64 = 1000;
/// Client standingsvc agent access: required standing for levels 2 through 5.
const AGENT_LEVEL_STANDINGS: [f64; 4] = [1.0, 3.0, 5.0, 7.0];
/// Client standingsvc agent access: level two.
const AGENT_LEVEL_TWO: i64 = 2;
/// Client standingsvc agent access: level three.
const AGENT_LEVEL_THREE: i64 = 3;
/// Client standingsvc agent access: level four.
const AGENT_LEVEL_FOUR: i64 = 4;

pub async fn standing(db: &Db, char_id: i64, from: i64) -> Result<f64, CallError> {
    Ok(db
        .query_opt("SELECT standing FROM game.standings WHERE char_id = $1 AND from_id = $2", &[&char_id, &from])
        .await
        .map_err(db_err)?
        .map(|r| r.get(0))
        .unwrap_or(0.0))
}

/// Move `from`'s standing toward the character `change` of the way to +10 (or -10 if negative).
pub async fn modify(db: &Db, world: &World, char_id: i64, from: i64, change: f64) -> Result<(), CallError> {
    if from == 0 || change == 0.0 {
        return Ok(());
    }
    let old = standing(db, char_id, from).await?;
    let new = if change > 0.0 { old + (STANDING_LIMIT - old) * change } else { old + (STANDING_LIMIT + old) * change }.clamp(-STANDING_LIMIT, STANDING_LIMIT);
    db
        .execute(
            "INSERT INTO game.standings (char_id, from_id, standing) VALUES ($1, $2, $3)
             ON CONFLICT (char_id, from_id) DO UPDATE SET standing = $3",
            &[&char_id, &from, &new],
        )
        .await
        .map_err(db_err)?;
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
    Ok(db
        .query_opt("SELECT amount FROM game.lp_balances WHERE char_id = $1 AND corp_id = $2", &[&char_id, &corp])
        .await
        .map_err(db_err)?
        .map(|r| r.get(0))
        .unwrap_or(0))
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
    db
        .execute(
            "INSERT INTO game.lp_balances (char_id, corp_id, amount) VALUES ($1, $2, $3)
             ON CONFLICT (char_id, corp_id) DO UPDATE SET amount = game.lp_balances.amount + $3",
            &[&char_id, &corp, &amount],
        )
        .await
        .map_err(db_err)?;
    outbox.notify_pilot(char_id, "OnLPChange", vec![Value::str("lp"), Value::Int(corp), Value::Int(before), Value::Int(before + amount)]);
    Ok(())
}

/// A corporation's LP store: (offerID = typeID, typeID, LP, ISK).
async fn offers(db: &Db, corp: i64) -> Result<Vec<(i32, i64, i64)>, CallError> {
    let rows = db
        .query(
            "SELECT t.type_id, r.average
             FROM sde_native.npccorporations c
             JOIN sde.inv_types t ON t.race_id = (c.data->>'raceID')::int
             JOIN sde.type_attributes a ON a.type_id = t.type_id
             JOIN sde.dogma_attributes d ON d.attribute_id = a.attribute_id AND d.name = 'metaGroupID'
             JOIN game.retail_prices r ON r.type_id = t.type_id AND r.average > 0
             WHERE c.key = $1::bigint::text AND a.value = $2 AND t.published AND t.market_group_id IS NOT NULL
             ORDER BY t.type_id",
            &[&corp, &META_GROUP_FACTION],
        )
        .await
        .map_err(db_err)?;
    Ok(rows
        .iter()
        .map(|r| {
            let lp = (((r.get::<_, f64>(1) / ISK_VALUE_PER_LP) / LP_PRICE_INCREMENT as f64).round() as i64 * LP_PRICE_INCREMENT).max(MIN_LP_PRICE);
            (r.get(0), lp, lp * ISK_PER_LP)
        })
        .collect())
}

/// `TakeOfferForCharacter(corpID, offerID, count)`: pay LP and ISK, get the items in the
/// station hangar.
async fn take_offer(c: &Client, corp: i64, offer: i64, count: i64) -> CallResult {
    let (db, world) = (&c.db, &*c.world);
    let me = c.session.int("charid").unwrap_or(0);
    let Some(station) = c.session.int("stationid") else {
        return Err(notify_error(messages::STATION_DOCK_REQUIRED_FOR_LP_STORE));
    };
    let count = count.clamp(1, MAX_OFFER_COUNT);
    let Some((type_id, lp, isk)) = offers(db, corp).await?.into_iter().find(|o| o.0 as i64 == offer) else {
        return Err(notify_error(messages::LP_OFFER_UNAVAILABLE));
    };
    let _trading = super::market::lock().await;
    db.transaction(
        |tx, outbox| Box::pin(async move {
            if lp_balance(tx, me, corp).await? < lp * count {
                return Err(notify_error(messages::INSUFFICIENT_LOYALTY_POINTS));
            }
            super::market::debit(tx, outbox, me, (isk * count) as f64).await?;
            add_lp(tx, outbox, me, corp, -lp * count).await?;
            super::market::deliver(tx, outbox, me, station, type_id, count as i32).await?;
            Ok(())
        }),
        |value, outbox| async move { dispatch_world(value, outbox, world) },
    ).await?;
    info!("lp store: {me} bought {count} x {type_id} from {corp} for {} LP and {} ISK", lp * count, isk * count);
    Ok(Value::Bool(true))
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let me = c.session.int("charid").unwrap_or(0);
    let int = |i: usize| call.args.get(i).and_then(|v| v.as_int()).unwrap_or(0);
    let db = &c.db;
    Some(match (call.service, call.method) {
        ("standingMgr", "GetCharStandings") => match db.query("SELECT from_id, standing FROM game.standings WHERE char_id = $1", &[&me]).await {
            Ok(rows) => Ok(rowset(&["fromID", "standing"], rows.iter().map(|r| vec![Value::Int(r.get(0)), Value::Float(r.get(1))]).collect())),
            Err(e) => Err(db_err(e)),
        },
        ("standingMgr", "GetCorpStandings") => Ok(rowset(&["fromID", "standing"], vec![])),
        ("standingMgr", "GetStandingTransactions") | ("standingMgr", "GetStandingCompositions") => Ok(Value::list([])),

        // [(issuerCorpID, amount)]
        ("LPSvc", "GetAllMyCharacterWalletLPBalances") => match db.query("SELECT corp_id, amount FROM game.lp_balances WHERE char_id = $1 AND amount > 0", &[&me]).await {
            Ok(rows) => Ok(Value::list(rows.iter().map(|r| Value::tuple([Value::Int(r.get(0)), Value::Int(r.get(1))])))),
            Err(e) => Err(db_err(e)),
        },
        ("LPSvc", "GetAllMyCorporationWalletLPBalances") => Ok(Value::list([])),
        ("LPStoreMgr", "GetAvailableOffersFromCorp") => {
            let corp = int(0);
            offers(db, corp).await.map(|list| {
                Value::list(list.into_iter().map(|(type_id, lp, isk)| {
                    keyval([
                        ("offerID", Value::Int(type_id as i64)),
                        ("corpID", Value::Int(corp)),
                        ("typeID", Value::Int(type_id as i64)),
                        ("qty", Value::Int(1)),
                        ("lpCost", Value::Int(lp)),
                        ("iskCost", Value::Int(isk)),
                        ("akCost", Value::Int(0)),
                        ("reqItems", Value::list([])),
                        ("requiredStandings", Value::None),
                    ])
                }))
            })
        }
        ("LPStoreMgr", "TakeOfferForCharacter") => take_offer(c, int(0), int(1), call.args.get(2).and_then(|v| v.as_int()).unwrap_or(1)).await,
        ("LPStoreMgr", "TakeOfferForCorporation") => Err(notify_error(messages::CORPORATION_LP_WALLET_UNSUPPORTED)),
        _ => return None,
    })
}
