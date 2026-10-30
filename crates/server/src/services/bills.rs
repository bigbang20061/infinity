//! Bills (billMgr; bound allianceRegistry's GetBills/PayBill): what corporations, alliances
//! and characters owe, in game.bills.
//!
//! Office rent is the one bill issued: a week before an office's rental period ends its
//! corporation is billed the station's rent, due when the period ends; paying extends the
//! office by another period. A corporation can have its accountants' chosen bill types paid
//! automatically from a wallet division (billMgr automatic payment settings). Billing runs
//! every ten minutes. An office whose rent goes unpaid isn't taken away.

use super::{db_err, has_role_or_is_ceo, keyval, notify_error, Call, CallError, CallResult};
use crate::dao::characters::members;
use crate::client::{filetime_now, Client};
use crate::consts::{ACCOUNT_CASH, FILETIME_DAY, RENTAL_DAYS, ROLE_ACCOUNTANT, ROLE_DIRECTOR};
use crate::db::Db;
use crate::db::outbox::dispatch_world;
use crate::messages;
use crate::space::World;
use marshal::Value;
use std::sync::Arc;
use tracing::{info, warn};

/// Existing server policy for billing maintenance cadence.
const BILLING_INTERVAL_SECONDS: u64 = 600;

// appConst.billType* and bill status
const BILL_RENTAL: i32 = 2;
const UNPAID: i32 = 0;
const PAID: i32 = 1;
/// Retained server advance-billing policy for rent before the period ends.
const BILL_AHEAD: i64 = 7 * FILETIME_DAY;

const BILL_SELECT: &str =
    "SELECT bill_id, bill_type, debtor_id, creditor_id, amount::float8, due, interest, external_id, external_id2 FROM game.bills";

fn bill_row(r: &tokio_postgres::Row) -> Value {
    let opt = |i: usize| r.get::<_, Option<i64>>(i).map(Value::Int).unwrap_or(Value::None);
    keyval([
        ("billID", Value::Int(r.get(0))),
        ("billTypeID", Value::Int(r.get::<_, i32>(1) as i64)),
        ("debtorID", Value::Int(r.get(2))),
        ("creditorID", Value::Int(r.get(3))),
        ("amount", Value::Float(r.get(4))),
        ("dueDateTime", Value::Int(r.get(5))),
        ("interest", Value::Float(r.get(6))),
        ("externalID", opt(7)),
        ("externalID2", opt(8)),
        ("paid", Value::Bool(false)),
    ])
}

/// Unpaid bills `owner` owes (`creditor` false) or is owed.
pub(super) async fn bills(db: &Db, owner: i64, creditor: bool) -> CallResult {
    let column = if creditor { "creditor_id" } else { "debtor_id" };
    let rows = db
        .query(&format!("{BILL_SELECT} WHERE {column} = $1 AND status = $2 ORDER BY due"), &[&owner, &UNPAID])
        .await
        .map_err(db_err)?;
    Ok(Value::list(rows.iter().map(bill_row)))
}

/// Tell `corp`'s online members their bills changed.
async fn announce(db: &Db, world: &World, corp: i64) -> Result<(), CallError> {
    for id in members::active_ids(db, corp).await.map_err(db_err)? {
        world.notify_pilot(id, "OnBillReceived", vec![]);
    }
    Ok(())
}

/// Pay a bill from `corp`'s division `key` (or, with `corp` None, the debtor character's
/// wallet), credit the creditor, and carry out what it paid for.
pub(super) async fn pay(db: &Db, world: &World, bill: i64, corp: Option<i64>, key: i32) -> Result<(), CallError> {
    let _trading = super::market::lock().await;
    db.transaction(
        |tx, outbox| Box::pin(async move {
            let Some(r) = tx.query_opt(&format!("{BILL_SELECT} WHERE bill_id = $1 AND status = $2"), &[&bill, &UNPAID]).await.map_err(db_err)? else {
                return Err(notify_error(messages::BILL_ALREADY_PAID_OR_MISSING));
            };
            let (kind, debtor, creditor, amount, external): (i32, i64, i64, f64, Option<i64>) = (r.get(1), r.get(2), r.get(3), r.get(4), r.get(7));
            match corp {
                Some(corp) => super::wallets::corp_debit(tx, outbox, corp, key, amount).await?,
                None => super::wallets::debit(tx, outbox, debtor, amount).await?,
            }
            if super::corps::is_player_corp(creditor) {
                super::wallets::corp_credit(tx, outbox, creditor, ACCOUNT_CASH, amount).await?;
            }
            tx.execute("UPDATE game.bills SET status = $2 WHERE bill_id = $1", &[&bill, &PAID]).await.map_err(db_err)?;
            if kind == BILL_RENTAL {
                if let Some(office) = external {
                    tx.execute(
                        "UPDATE game.offices SET rented_until = rented_until + $2 WHERE office_id = $1",
                        &[&office, &(RENTAL_DAYS * FILETIME_DAY)],
                    ).await.map_err(db_err)?;
                }
            }
            if let Some(corp) = corp {
                for id in members::active_ids(tx, corp).await.map_err(db_err)? {
                    outbox.notify_pilot(id, "OnBillReceived", vec![]);
                }
            }
            Ok((debtor, creditor, amount))
        }),
        |(debtor, creditor, amount), outbox| async move {
            dispatch_world((), outbox, world)?;
            info!("bills: {debtor} paid bill {bill} ({amount:.0} ISK to {creditor})");
            Ok(())
        },
    ).await
}

/// Issue rent bills that have come due and pay those set to be paid automatically.
async fn run(db: &Db, world: &World) -> Result<(), CallError> {
    let now = filetime_now();
    let due = db
        .query(
            "SELECT o.office_id, o.corporation_id, o.station_id, o.rented_until, st.owner_id::bigint FROM game.offices o
             LEFT JOIN sde.sta_stations st ON st.station_id = o.station_id
             WHERE o.rented_until - $1 <= $2
               AND NOT EXISTS (SELECT 1 FROM game.bills b WHERE b.bill_type = $3 AND b.external_id = o.office_id AND b.status = $4)",
            &[&now, &BILL_AHEAD, &BILL_RENTAL, &UNPAID],
        )
        .await
        .map_err(db_err)?;
    for r in due {
        let (office, corp, station, until): (i64, i64, i64, i64) = (r.get(0), r.get(1), r.get(2), r.get(3));
        let landlord = r.get::<_, Option<i64>>(4).unwrap_or(1_000_001);
        let amount = super::offices::rent_price(db, station).await?;
        db
            .execute(
                "INSERT INTO game.bills (bill_type, debtor_id, creditor_id, amount, due, external_id, external_id2, created)
                 VALUES ($1, $2, $3, $4::float8::numeric, $5, $6, $7, $8)",
                &[&BILL_RENTAL, &corp, &landlord, &amount, &until, &office, &station, &now],
            )
            .await
            .map_err(db_err)?;
        info!("bills: billed {corp} {amount:.0} ISK rent for office {office}");
        announce(db, world, corp).await?;
    }
    let auto = db
        .query(
            "SELECT b.bill_id, b.debtor_id, co.bill_division FROM game.bills b
             JOIN game.bill_autopay a ON a.owner_id = b.debtor_id AND a.bill_type = b.bill_type AND a.enabled
             JOIN game.corporations co ON co.corporation_id = b.debtor_id
             WHERE b.status = $1",
            &[&UNPAID],
        )
        .await
        .map_err(db_err)?;
    for r in auto {
        let (bill, corp, key): (i64, i64, i32) = (r.get(0), r.get(1), r.get(2));
        if let Err(e) = pay(db, world, bill, Some(corp), key).await {
            if let CallError::Internal(e) = e {
                warn!("bills: paying {bill} automatically: {e:#}");
            }
        }
    }
    Ok(())
}

/// Billing, every ten minutes.
pub async fn run_billing(db: Db, world: Arc<World>) {
    loop {
        if let Err(CallError::Internal(e)) = run(&db, &world).await {
            warn!("billing failed: {e:#}");
        }
        tokio::time::sleep(std::time::Duration::from_secs(BILLING_INTERVAL_SECONDS)).await;
    }
}

/// An accountant, director or the CEO of `corp`.
pub(super) async fn is_accountant(db: &Db, me: i64, corp: i64) -> Result<bool, CallError> {
    has_role_or_is_ceo(db, me, corp, ROLE_ACCOUNTANT | ROLE_DIRECTOR).await
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service != "billMgr" {
        return None;
    }
    let me = c.session.int("charid").unwrap_or(0);
    let corp = c.session.int("corpid").unwrap_or(0);
    let (db, world) = (&c.db, &*c.world);
    let int = |i: usize| call.args.get(i).and_then(|v| v.as_int());
    Some(match call.method {
        "GetCorporationBills" => bills(db, corp, false).await,
        "GetCorporationBillsReceivable" => bills(db, corp, true).await,
        // PayCorporationBill(billID, fromAccountKey=...)
        "PayCorporationBill" => {
            let key = call.kwargs.get("fromAccountKey").and_then(|v| v.as_int()).or(int(1)).unwrap_or(ACCOUNT_CASH as i64) as i32;
            match (is_accountant(db, me, corp).await, super::corpwallet::may_take(db, me, corp, key).await) {
                (Ok(true), Ok(true)) => pay(db, world, int(0).unwrap_or(0), Some(corp), key).await.map(|_| Value::None),
                (Err(e), _) | (_, Err(e)) => Err(e),
                _ => Err(notify_error(messages::BILL_PAYMENT_PERMISSION_REQUIRED)),
            }
        }
        "CharPayBill" => {
            let bill = int(0).unwrap_or(0);
            match db.query_opt("SELECT 1 FROM game.bills WHERE bill_id = $1 AND debtor_id = $2", &[&bill, &me]).await {
                Ok(Some(_)) => pay(db, world, bill, None, 0).await.map(|_| Value::None),
                Ok(None) => Err(notify_error(messages::BILL_NOT_OWNED_BY_CHARACTER)),
                Err(e) => Err(db_err(e)),
            }
        }
        // {ownerID: {billTypeID: bool, 'divisionID': accountKey}}
        "GetAutomaticPaySettings" => {
            let rows = match db.query("SELECT bill_type, enabled FROM game.bill_autopay WHERE owner_id = $1", &[&corp]).await {
                Ok(rows) => rows,
                Err(e) => return Some(Err(db_err(e))),
            };
            let division: i32 = match db.query_opt("SELECT bill_division FROM game.corporations WHERE corporation_id = $1", &[&corp]).await {
                Ok(r) => r.map(|r| r.get(0)).unwrap_or(ACCOUNT_CASH),
                Err(e) => return Some(Err(db_err(e))),
            };
            let mut settings: Vec<(Value, Value)> = rows.iter().map(|r| (Value::Int(r.get::<_, i32>(0) as i64), Value::Bool(r.get(1)))).collect();
            settings.push((Value::str("divisionID"), Value::Int(division as i64)));
            Ok(Value::Dict(vec![(Value::Int(corp), Value::Dict(settings))]))
        }
        // SendAutomaticPaySettings({ownerID: {billTypeID: bool, 'divisionID': accountKey}})
        "SendAutomaticPaySettings" => {
            match is_accountant(db, me, corp).await {
                Ok(true) => {}
                Ok(false) => return Some(Err(notify_error(messages::ACCOUNTANT_REQUIRED_FOR_AUTOPAY_SETTINGS))),
                Err(e) => return Some(Err(e)),
            }
            let Some(Value::Dict(owners)) = call.args.first() else { return Some(Ok(Value::None)) };
            for (owner, settings) in owners {
                if owner.as_int() != Some(corp) {
                    continue;
                }
                let Value::Dict(settings) = settings else { continue };
                for (k, v) in settings {
                    let r = match (k.as_int(), k.as_text().as_deref()) {
                        (Some(kind), _) => {
                            let enabled = matches!(v, Value::Bool(true)) || v.as_int().is_some_and(|i| i != 0);
                            db
                                .execute(
                                    "INSERT INTO game.bill_autopay (owner_id, bill_type, enabled) VALUES ($1, $2, $3)
                                     ON CONFLICT (owner_id, bill_type) DO UPDATE SET enabled = $3",
                                    &[&corp, &(kind as i32), &enabled],
                                )
                                .await
                        }
                        (None, Some("divisionID")) => {
                            let key = v.as_int().unwrap_or(ACCOUNT_CASH as i64) as i32;
                            db.execute("UPDATE game.corporations SET bill_division = $2 WHERE corporation_id = $1", &[&corp, &key]).await
                        }
                        _ => continue,
                    };
                    if let Err(e) = r {
                        return Some(Err(db_err(e)));
                    }
                }
            }
            Ok(Value::None)
        }
        _ => return None,
    })
}
