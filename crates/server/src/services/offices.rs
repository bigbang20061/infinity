//! Corporation offices (officeManager, bound per station): a player corporation rents an
//! office at a station from its master wallet for appConst.rentalPeriodOffice days at a
//! time. The office is an item (typeOffice) owned by the corporation at the station, and
//! the corporation hangar divisions are its contents under flagCorpSAG1-7 (services/
//! inventory lists and moves them). Renting again before the period ends extends it;
//! renewing is billed (services/bills).

use super::corps::is_player_corp;
use super::{db_err, has_role_or_is_ceo, keyval, notify_error, Call, CallError, CallResult};
use crate::dao::characters::members;
use crate::client::{filetime_now, Client};
use crate::consts::{ACCOUNT_CASH, FILETIME_DAY, RENTAL_DAYS, ROLE_DIRECTOR};
use crate::db::Db;
use crate::db::executor::Executor;
use crate::db::outbox::dispatch_world;
use crate::messages;
use crate::space::World;
use marshal::Value;
use tracing::info;

/// SDE invTypes: Office.
const TYPE_OFFICE: i32 = 27;
/// Server office item policy: no inventory flag on the station's office item.
const OFFICE_ITEM_FLAG: i32 = 0;
/// Offices a station has room for (the live server's capacity never shipped with the client or SDE).
const STATION_OFFICE_SLOTS: i64 = 24;
/// appConst.corpRoleCanRentOffice
const ROLE_CAN_RENT_OFFICE: i64 = 562_949_953_421_312;
/// Server office rent policy: security used when the station's system is unavailable.
const DEFAULT_SECURITY: f64 = 0.5;
/// Server office rent policy: minimum rent in ISK.
const BASE_RENT: f64 = 100_000.0;
/// Server office rent policy: additional ISK at security 1.
const SECURITY_RENT: f64 = 900_000.0;
/// Server office rent policy: half-ISK tolerance for rounded quotes.
const QUOTE_TOLERANCE: f64 = 0.5;

/// The rent for one period at `station`: dearer in busier, higher-security space.
pub(super) async fn rent_price(db: &Db, station: i64) -> Result<f64, CallError> {
    let security: f64 = db
        .query_opt(
            "SELECT COALESCE(s.security, 0)::float8 FROM sde.sta_stations st JOIN sde.map_solar_systems s ON s.solar_system_id = st.solar_system_id
             WHERE st.station_id = $1",
            &[&(station as i32)],
        )
        .await
        .map_err(db_err)?
        .map(|r| r.get(0))
        .unwrap_or(DEFAULT_SECURITY);
    Ok((BASE_RENT + SECURITY_RENT * security.clamp(0.0, 1.0)).round())
}

/// The corporation's office at `station`, if it rents one.
pub(super) async fn office_at(db: &(impl Executor + ?Sized), corp: i64, station: i64) -> Result<Option<i64>, CallError> {
    Ok(db
        .query_opt("SELECT office_id FROM game.offices WHERE corporation_id = $1 AND station_id = $2", &[&corp, &station])
        .await
        .map_err(db_err)?
        .map(|r| r.get(0)))
}

/// Whether `item` is an office, and whose.
pub(super) async fn office_owner(db: &Db, item: i64) -> Result<Option<i64>, CallError> {
    Ok(db.query_opt("SELECT corporation_id FROM game.offices WHERE office_id = $1", &[&item]).await.map_err(db_err)?.map(|r| r.get(0)))
}
/// Tell the corporation's online members its offices changed.
async fn announce(db: &Db, world: &World, corp: i64, office: i64) -> Result<(), CallError> {
    for id in members::active_ids(db, corp).await.map_err(db_err)? {
        world.notify_pilot(id, "OnOfficeRentalChange", vec![Value::Int(corp), Value::Int(office)]);
    }
    Ok(())
}


/// Rent (or extend) `corp`'s office at `station` for a period, paid from its master wallet.
pub(super) async fn rent(db: &Db, world: &World, corp: i64, station: i64) -> Result<i64, CallError> {
    let price = rent_price(db, station).await?;
    let _trading = super::market::lock().await;
    let now = filetime_now();
    db.transaction(
        |tx, outbox| Box::pin(async move {
            super::wallets::corp_debit(tx, outbox, corp, ACCOUNT_CASH, price).await?;
            if let Some(office) = office_at(tx, corp, station).await? {
                tx.execute("UPDATE game.offices SET rented_until = GREATEST(rented_until, $2) + $3 WHERE office_id = $1", &[&office, &now, &(RENTAL_DAYS * FILETIME_DAY)])
                    .await.map_err(db_err)?;
                return Ok((office, false));
            }
            let office: i64 = tx.query_one(
                "INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) VALUES ($1, $2, $3, $4, 1, true) RETURNING item_id",
                &[&TYPE_OFFICE, &corp, &station, &OFFICE_ITEM_FLAG],
            ).await.map_err(db_err)?.get(0);
            tx.execute(
                "INSERT INTO game.offices (office_id, corporation_id, station_id, rented_until) VALUES ($1, $2, $3, $4)",
                &[&office, &corp, &station, &(now + RENTAL_DAYS * FILETIME_DAY)],
            ).await.map_err(db_err)?;
            for id in members::active_ids(tx, corp).await.map_err(db_err)? {
                outbox.notify_pilot(id, "OnOfficeRentalChange", vec![Value::Int(corp), Value::Int(office)]);
            }
            Ok((office, true))
        }),
        |(office, created), outbox| async move {
            dispatch_world((), outbox, world)?;
            if created {
                info!("offices: {corp} rented office {office} at {station} for {price:.0} ISK");
            }
            Ok(office)
        },
    ).await
}

/// A director, the CEO, or a member allowed to rent offices.
async fn may_rent(db: &Db, me: i64, corp: i64) -> Result<bool, CallError> {
    has_role_or_is_ceo(db, me, corp, ROLE_DIRECTOR | ROLE_CAN_RENT_OFFICE).await
}

/// The office rows the client keeps (officeID, stationID, solarsystemID, stationTypeID).
async fn my_offices(db: &Db, corp: i64) -> CallResult {
    let rows = db
        .query(
            "SELECT o.office_id, o.station_id, st.solar_system_id::bigint, st.type_id::bigint, o.rented_until FROM game.offices o
             LEFT JOIN sde.sta_stations st ON st.station_id = o.station_id WHERE o.corporation_id = $1 ORDER BY o.office_id",
            &[&corp],
        )
        .await
        .map_err(db_err)?;
    let opt = |v: Option<i64>| v.map(Value::Int).unwrap_or(Value::None);
    Ok(Value::list(rows.iter().map(|r| {
        keyval([
            ("officeID", Value::Int(r.get(0))),
            ("stationID", Value::Int(r.get(1))),
            ("solarsystemID", opt(r.get(2))),
            ("stationTypeID", opt(r.get(3))),
            ("typeID", Value::Int(TYPE_OFFICE as i64)),
            ("rentedUntil", Value::Int(r.get(4))),
        ])
    })))
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service != "officeManager" {
        return None;
    }
    let me = c.session.int("charid").unwrap_or(0);
    let corp = c.session.int("corpid").unwrap_or(0);
    let (db, world) = (&c.db, &*c.world);
    if call.method == "GetMyCorporationsOffices" {
        return Some(my_offices(db, corp).await);
    }
    // Bound to a station.
    let station = call
        .bind_params
        .and_then(|p| p.as_int().or_else(|| p.items().and_then(|t| t.first()?.as_int())))
        .or_else(|| c.session.int("stationid"))?;
    Some(match call.method {
        // [corporationID] with an office here.
        "GetCorporationsWithOffices" => match db.query("SELECT corporation_id FROM game.offices WHERE station_id = $1", &[&station]).await {
            Ok(rows) => Ok(Value::list(rows.iter().map(|r| Value::Int(r.get(0))))),
            Err(e) => Err(db_err(e)),
        },
        "GetEmptyOfficeCount" => match db.query_one("SELECT count(*) FROM game.offices WHERE station_id = $1", &[&station]).await {
            Ok(r) => Ok(Value::Int((STATION_OFFICE_SLOTS - r.get::<_, i64>(0)).max(0))),
            Err(e) => Err(db_err(e)),
        },
        // GetPriceQuote(corpID) -> ISK for one rental period
        "GetPriceQuote" => rent_price(db, station).await.map(Value::Float),
        // RentOffice(cost): the quote the player agreed to.
        "RentOffice" => {
            if !is_player_corp(corp) {
                return Some(Err(notify_error(messages::NPC_CORPORATION_CANNOT_RENT_OFFICE)));
            }
            match may_rent(db, me, corp).await {
                Ok(true) => {}
                Ok(false) => return Some(Err(notify_error(messages::NO_OFFICE_RENTAL_ROLE))),
                Err(e) => return Some(Err(e)),
            }
            let quoted = call.args.first().and_then(|v| v.as_float().or_else(|| v.as_int().map(|i| i as f64)));
            match rent_price(db, station).await {
                Ok(price) if quoted.is_some_and(|q| q + QUOTE_TOLERANCE < price) => return Some(Err(notify_error(messages::OFFICE_RENT_CHANGED))),
                Err(e) => return Some(Err(e)),
                _ => {}
            }
            rent(db, world, corp, station).await.map(|_| Value::None)
        }
        // Give the office up; its hangars must be empty first.
        "UnrentOffice" => {
            match may_rent(db, me, corp).await {
                Ok(true) => {}
                Ok(false) => return Some(Err(notify_error(messages::NO_OFFICE_RELEASE_ROLE))),
                Err(e) => return Some(Err(e)),
            }
            let office = match office_at(db, corp, station).await {
                Ok(Some(o)) => o,
                Ok(None) => return Some(Ok(Value::None)),
                Err(e) => return Some(Err(e)),
            };
            match db.query_opt("SELECT 1 FROM game.items WHERE location_id = $1 LIMIT 1", &[&office]).await {
                Ok(Some(_)) => return Some(Err(notify_error(messages::EMPTY_CORPORATION_HANGAR_BEFORE_RELEASE))),
                Err(e) => return Some(Err(db_err(e))),
                Ok(None) => {}
            }
            let removed = async {
                db.execute("DELETE FROM game.offices WHERE office_id = $1", &[&office]).await.map_err(db_err)?;
                db.execute("DELETE FROM game.items WHERE item_id = $1", &[&office]).await.map_err(db_err)?;
                announce(db, world, corp, office).await?;
                info!("offices: {corp} gave up office {office} at {station}");
                Ok::<_, CallError>(Value::None)
            }
            .await;
            removed
        }
        // Nothing is impounded: an office is only given up empty.
        "PrimeOfficeItem" => Ok(Value::None),
        "HasCorpImpoundedItems" => Ok(Value::Bool(false)),
        "GetImpoundReleasePrice" => Ok(Value::None),
        _ => return None,
    })
}
