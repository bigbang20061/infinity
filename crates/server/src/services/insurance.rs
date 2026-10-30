//! Ship insurance (insuranceSvc, bound per station; eve/client/script/ui/station/insurance,
//! eve/common/script/util/insuranceConst.py).
//!
//! A ship's insured value is its retail adjusted price (else its base price). A contract
//! (game.insurance) pays out `fraction` of that value when the ship is destroyed and costs
//! (fraction - 0.4) / 2 of it up front, as insuranceConst prices its packages from Basic
//! (0.5) to Platinum (1.0); it runs for 12 weeks. Destroyed ships pay their owner and post an
//! insurance payout notification.

use super::{db_err, keyval, notify_error, value_id_list, Call, CallError, CallResult};
use crate::client::{filetime_now, Client};
use crate::consts::{CATEGORY_SHIP, FILETIME_DAY};
use crate::db::Db;
use crate::space::World;
use marshal::Value;
use tracing::info;

/// Server insurance policy: twelve-week contract duration.
const CONTRACT_DAYS: i64 = 84;
/// Client insuranceConst package price base fraction.
const BASE_FRACTION: f64 = 0.4;
/// Client insuranceConst package price multiplier.
const FRACTION_INCREMENT: f64 = 0.5;
/// Client insuranceConst insurance package payout fractions.
const FRACTIONS: [f64; 6] = [0.5, 0.6, 0.7, 0.8, 0.9, 1.0];
/// Client notificationconst insurance payout type.
const NOTIFICATION_PAYOUT: i32 = 35;
/// Client notificationconst insurance issued type.
const NOTIFICATION_ISSUED: i32 = 55;
/// Client notificationconst insurance group.
const NOTIFICATION_GROUP_INSURANCE: i32 = 11;
/// SDE NPC corporations: Secure Commerce Commission signs insurance.
const INSURER: i64 = 1000132;
/// Server insurance quote policy: permitted absolute difference in ISK.
const QUOTE_TOLERANCE_ISK: f64 = 1.0;

/// What a ship type is insured for (the payout at fraction 1.0); 0 if it can't be insured.
async fn value(db: &Db, type_id: i32) -> Result<f64, CallError> {
    Ok(db
        .query_opt(
            "SELECT COALESCE(NULLIF(r.adjusted, 0), NULLIF(r.average, 0), t.base_price, 0)::float8
             FROM sde.inv_types t LEFT JOIN game.retail_prices r USING (type_id) WHERE t.type_id = $1 AND t.category_id = $2",
            &[&type_id, &CATEGORY_SHIP],
        )
        .await
        .map_err(db_err)?
        .map(|r| r.get(0))
        .unwrap_or(0.0))
}

fn contract_row(r: &tokio_postgres::Row) -> Value {
    keyval([
        ("shipID", Value::Int(r.get(0))),
        ("ownerID", Value::Int(r.get(1))),
        ("fraction", Value::Float(r.get(2))),
        ("startDate", Value::Int(r.get(3))),
        ("endDate", Value::Int(r.get(4))),
    ])
}

const CONTRACT_SELECT: &str = "SELECT ship_id, owner_id, fraction, start_date, end_date FROM game.insurance";

/// `InsureShip(itemID, amount, isCorpItem, voidOld=False)`.
async fn insure(c: &Client, call: &Call<'_>) -> CallResult {
    let (db, world) = (&c.db, &*c.world);
    let me = c.session.int("charid").unwrap_or(0);
    let ship = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
    let amount = call.args.get(1).and_then(|v| v.as_float()).unwrap_or(0.0);
    let void_old = call.kwargs.get("voidOld").or(call.args.get(3)).is_some_and(|v| matches!(v, Value::Bool(true)) || v.as_int() == Some(1));
    let Some(item) = super::dogma::load_items_db(db, "i.item_id = $1", &[&ship]).await?.pop() else {
        return Err(CallError::User { msg: "InsCouldNotFindItem".into(), dict: vec![] });
    };
    if item.owner_id != me || !item.singleton || item.category_id != CATEGORY_SHIP as i64 {
        return Err(notify_error("只能为你自己已组装的舰船投保。"));
    }
    let full = value(db, item.type_id).await?;
    let Some(fraction) = FRACTIONS.iter().copied().find(|f| (full * (f - BASE_FRACTION) * FRACTION_INCREMENT - amount).abs() < QUOTE_TOLERANCE_ISK) else {
        return Err(notify_error("保险报价已过期，请重新打开窗口。"));
    };
    let existing = db.query_opt(&format!("{CONTRACT_SELECT} WHERE ship_id = $1"), &[&ship]).await.map_err(db_err)?;
    if existing.is_some() && !void_old {
        return Err(CallError::User { msg: "InsureShipFailedSingleContract".into(), dict: vec![("ownerID".into(), Value::Int(me))] });
    }
    super::market::debit(db, world, me, amount).await?;
    let now = filetime_now();
    db
        .execute(
            "INSERT INTO game.insurance (ship_id, owner_id, type_id, fraction, start_date, end_date) VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (ship_id) DO UPDATE SET owner_id = $2, type_id = $3, fraction = $4, start_date = $5, end_date = $6",
            &[&ship, &me, &item.type_id, &fraction, &now, &(now + CONTRACT_DAYS * FILETIME_DAY)],
        )
        .await
        .map_err(db_err)?;
    info!("insurance: {me} insured ship {ship} at {fraction} for {amount:.0} ISK");
    super::mail::notify(db, world, me, NOTIFICATION_ISSUED, NOTIFICATION_GROUP_INSURANCE, INSURER, &format!("{{itemID: {ship}, typeID: {}}}", item.type_id)).await?;
    world.notify_pilot(me, "OnShipInsured", vec![]);
    Ok(Value::None)
}

/// A ship was destroyed: pay out its insurance, if it has any that is still running.
pub async fn ship_destroyed(db: &Db, world: &World, ship: i64) -> Result<(), CallError> {
    let Some(r) = db
        .query_opt("DELETE FROM game.insurance WHERE ship_id = $1 RETURNING owner_id, type_id, fraction, end_date", &[&ship])
        .await
        .map_err(db_err)?
    else {
        return Ok(());
    };
    let (owner, type_id, fraction, end): (i64, i32, f64, i64) = (r.get(0), r.get(1), r.get(2), r.get(3));
    if end < filetime_now() {
        return Ok(());
    }
    let payout = super::market::round2(value(db, type_id).await? * fraction);
    super::market::credit(db, world, owner, payout).await?;
    let data = format!("{{amount: {payout}, itemID: {ship}, typeID: {type_id}, payout: 0}}");
    super::mail::notify(db, world, owner, NOTIFICATION_PAYOUT, NOTIFICATION_GROUP_INSURANCE, INSURER, &data).await?;
    info!("insurance: ship {ship} of {owner} paid out {payout:.0} ISK");
    Ok(())
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service != "insuranceSvc" {
        return None;
    }
    let me = c.session.int("charid").unwrap_or(0);
    let db = &c.db;
    Some(match call.method {
        "GetInsurancePrice" => value(db, call.args.first().and_then(|v| v.as_int()).unwrap_or(0) as i32).await.map(Value::Float),
        "GetInsurancePrices" => {
            let types = value_id_list(call.args.first());
            let mut out = vec![];
            for t in types {
                match value(db, t as i32).await {
                    Ok(v) => out.push((Value::Int(t), Value::Float(v))),
                    Err(e) => return Some(Err(e)),
                }
            }
            Ok(Value::Dict(out))
        }
        // GetContracts(includeCorp=0): the character's (corporations insure nothing yet).
        "GetContracts" => {
            if call.args.first().and_then(|v| v.as_int()).unwrap_or(0) != 0 {
                return Some(Ok(Value::list([])));
            }
            match db.query(&format!("{CONTRACT_SELECT} WHERE owner_id = $1 AND end_date > $2"), &[&me, &filetime_now()]).await {
                Ok(rows) => Ok(Value::list(rows.iter().map(contract_row))),
                Err(e) => Err(db_err(e)),
            }
        }
        "GetContractForShip" => {
            let ship = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
            match db.query_opt(&format!("{CONTRACT_SELECT} WHERE ship_id = $1 AND end_date > $2"), &[&ship, &filetime_now()]).await {
                Ok(r) => Ok(r.map(|r| contract_row(&r)).unwrap_or(Value::None)),
                Err(e) => Err(db_err(e)),
            }
        }
        "InsureShip" => insure(c, call).await,
        "UnInsureShip" => {
            let ship = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
            match db.execute("DELETE FROM game.insurance WHERE ship_id = $1 AND owner_id = $2", &[&ship, &me]).await {
                Ok(_) => {
                    c.world.notify_pilot(me, "OnShipInsured", vec![]);
                    Ok(Value::None)
                }
                Err(e) => Err(db_err(e)),
            }
        }
        _ => return None,
    })
}
