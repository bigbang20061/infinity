//! LPStoreMgr answers station/loyaltyPointStore using SDE and game retail prices.
//! Live LP offer tables are unavailable; offers are derived from retail data.
use super::{lp_balance, add_lp};
use crate::services::{db_err, keyval, notify_error, Call, CallError, CallResult};
use crate::client::Client;
use crate::db::{Db, outbox::dispatch_world};
use crate::messages;
use marshal::Value;
use tracing::info;
/// Server LP store policy: retail ISK value per LP.
const ISK_VALUE_PER_LP: f64 = 2000.0;
/// Server LP store policy: ISK charged per LP.
const ISK_PER_LP: i64 = 1000;
/// SDE metaGroups: faction.
const META_GROUP_FACTION: f64 = 4.0;
/// Server LP store policy: offer prices are rounded to this LP increment.
const LP_PRICE_INCREMENT: i64 = 25;
/// Server LP store policy: minimum LP price.
const MIN_LP_PRICE: i64 = 100;
/// Server LP store policy: maximum quantity per purchase.
const MAX_OFFER_COUNT: i64 = 1000;

/// A corporation's LP store: (offerID = typeID, typeID, LP, ISK).
async fn offers(db: &Db, corp: i64) -> Result<Vec<(i32, i64, i64)>, CallError> {
    let rows = crate::dao::store::loyalty::offers(db, corp, META_GROUP_FACTION).await.map_err(db_err)?;
    Ok(rows
        .iter()
        .map(|r| {
            let lp = (((r.average / ISK_VALUE_PER_LP) / LP_PRICE_INCREMENT as f64).round() as i64 * LP_PRICE_INCREMENT).max(MIN_LP_PRICE);
            (r.type_id, lp, lp * ISK_PER_LP)
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
    let _trading = crate::services::market::lock().await;
    db.transaction(
        |tx, outbox| Box::pin(async move {
            if lp_balance(tx, me, corp).await? < lp * count {
                return Err(notify_error(messages::INSUFFICIENT_LOYALTY_POINTS));
            }
            crate::services::wallets::debit(tx, outbox, me, (isk * count) as f64).await?;
            add_lp(tx, outbox, me, corp, -lp * count).await?;
            crate::services::items::deliver(tx, outbox, me, station, type_id, count as i32).await?;
            Ok(())
        }),
        |value, outbox| async move { dispatch_world(value, outbox, world) },
    ).await?;
    info!("lp store: {me} bought {count} x {type_id} from {corp} for {} LP and {} ISK", lp * count, isk * count);
    Ok(Value::Bool(true))
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let int = |i: usize| call.args.get(i).and_then(|v| v.as_int()).unwrap_or(0);
    let db = &c.db;
    Some(match call.method {
        "GetAvailableOffersFromCorp" => {
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
        "TakeOfferForCharacter" => take_offer(c, int(0), int(1), call.args.get(2).and_then(|v| v.as_int()).unwrap_or(1)).await,
        "TakeOfferForCorporation" => Err(notify_error(messages::CORPORATION_LP_WALLET_UNSUPPORTED)),
        _ => return None,
    })
}
