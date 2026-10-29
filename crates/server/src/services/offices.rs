//! Corporation offices (officeManager, bound per station): a player corporation rents an
//! office at a station from its master wallet for appConst.rentalPeriodOffice days at a
//! time. The office is an item (typeOffice) owned by the corporation at the station, and
//! the corporation hangar divisions are its contents under flagCorpSAG1-7 (services/
//! inventory lists and moves them). Renting again before the period ends extends it;
//! renewing is billed (services/bills).

use super::corps::{ceo_of, is_player_corp, membership};
use super::{db_err, keyval, notify_error, Call, CallError, CallResult};
use crate::client::{filetime_now, Client};
use crate::consts::{ACCOUNT_CASH, FILETIME_DAY, RENTAL_DAYS, ROLE_DIRECTOR};
use crate::db::Db;
use crate::space::World;
use marshal::Value;
use tracing::info;

pub(super) const TYPE_OFFICE: i32 = 27;
/// Offices a station has room for (the live server's capacity never shipped with the client or SDE).
const STATION_OFFICE_SLOTS: i64 = 24;
/// appConst.corpRoleCanRentOffice
const ROLE_CAN_RENT_OFFICE: i64 = 562_949_953_421_312;

/// The rent for one period at `station`: dearer in busier, higher-security space.
pub(super) async fn rent_price(db: &Db, station: i64) -> Result<f64, CallError> {
    let security: f64 = db
        .client
        .query_opt(
            "SELECT COALESCE(s.security, 0)::float8 FROM sde.sta_stations st JOIN sde.map_solar_systems s ON s.solar_system_id = st.solar_system_id
             WHERE st.station_id = $1",
            &[&(station as i32)],
        )
        .await
        .map_err(db_err)?
        .map(|r| r.get(0))
        .unwrap_or(0.5);
    Ok((100_000.0 + 900_000.0 * security.clamp(0.0, 1.0)).round())
}

/// The corporation's office at `station`, if it rents one.
pub(super) async fn office_at(db: &Db, corp: i64, station: i64) -> Result<Option<i64>, CallError> {
    Ok(db
        .client
        .query_opt("SELECT office_id FROM game.offices WHERE corporation_id = $1 AND station_id = $2", &[&corp, &station])
        .await
        .map_err(db_err)?
        .map(|r| r.get(0)))
}

/// Whether `item` is an office, and whose.
pub(super) async fn office_owner(db: &Db, item: i64) -> Result<Option<i64>, CallError> {
    Ok(db.client.query_opt("SELECT corporation_id FROM game.offices WHERE office_id = $1", &[&item]).await.map_err(db_err)?.map(|r| r.get(0)))
}

/// Tell the corporation's online members its offices changed.
async fn announce(db: &Db, world: &World, corp: i64, office: i64) -> Result<(), CallError> {
    let rows = db.client.query("SELECT character_id FROM game.characters WHERE corporation_id = $1 AND NOT deleted", &[&corp]).await.map_err(db_err)?;
    for r in rows {
        world.notify_pilot(r.get(0), "OnOfficeRentalChange", vec![Value::Int(corp), Value::Int(office)]);
    }
    Ok(())
}

/// Rent (or extend) `corp`'s office at `station` for a period, paid from its master wallet.
pub(super) async fn rent(db: &Db, world: &World, corp: i64, station: i64) -> Result<i64, CallError> {
    let price = rent_price(db, station).await?;
    let _trading = super::market::lock().await;
    super::corpwallet::corp_debit(db, world, corp, ACCOUNT_CASH, price).await?;
    let now = filetime_now();
    if let Some(office) = office_at(db, corp, station).await? {
        db.client
            .execute("UPDATE game.offices SET rented_until = GREATEST(rented_until, $2) + $3 WHERE office_id = $1", &[&office, &now, &(RENTAL_DAYS * FILETIME_DAY)])
            .await
            .map_err(db_err)?;
        return Ok(office);
    }
    let office: i64 = db
        .client
        .query_one(
            "INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) VALUES ($1, $2, $3, 0, 1, true) RETURNING item_id",
            &[&TYPE_OFFICE, &corp, &station],
        )
        .await
        .map_err(db_err)?
        .get(0);
    db.client
        .execute(
            "INSERT INTO game.offices (office_id, corporation_id, station_id, rented_until) VALUES ($1, $2, $3, $4)",
            &[&office, &corp, &station, &(now + RENTAL_DAYS * FILETIME_DAY)],
        )
        .await
        .map_err(db_err)?;
    announce(db, world, corp, office).await?;
    info!("offices: {corp} rented office {office} at {station} for {price:.0} ISK");
    Ok(office)
}

/// A director, the CEO, or a member allowed to rent offices.
async fn may_rent(db: &Db, me: i64, corp: i64) -> Result<bool, CallError> {
    let (my_corp, roles) = membership(db, me).await?;
    Ok(my_corp == corp && (roles & (ROLE_DIRECTOR | ROLE_CAN_RENT_OFFICE) != 0 || ceo_of(db, corp).await? == Some(me)))
}

/// The office rows the client keeps (officeID, stationID, solarsystemID, stationTypeID).
async fn my_offices(db: &Db, corp: i64) -> CallResult {
    let rows = db
        .client
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
        "GetCorporationsWithOffices" => match db.client.query("SELECT corporation_id FROM game.offices WHERE station_id = $1", &[&station]).await {
            Ok(rows) => Ok(Value::list(rows.iter().map(|r| Value::Int(r.get(0))))),
            Err(e) => Err(db_err(e)),
        },
        "GetEmptyOfficeCount" => match db.client.query_one("SELECT count(*) FROM game.offices WHERE station_id = $1", &[&station]).await {
            Ok(r) => Ok(Value::Int((STATION_OFFICE_SLOTS - r.get::<_, i64>(0)).max(0))),
            Err(e) => Err(db_err(e)),
        },
        // GetPriceQuote(corpID) -> ISK for one rental period
        "GetPriceQuote" => rent_price(db, station).await.map(Value::Float),
        // RentOffice(cost): the quote the player agreed to.
        "RentOffice" => {
            if !is_player_corp(corp) {
                return Some(Err(notify_error("NPC 军团不能租用办公室。")));
            }
            match may_rent(db, me, corp).await {
                Ok(true) => {}
                Ok(false) => return Some(Err(notify_error("你没有租用办公室的职权。"))),
                Err(e) => return Some(Err(e)),
            }
            let quoted = call.args.first().and_then(|v| v.as_float().or_else(|| v.as_int().map(|i| i as f64)));
            match rent_price(db, station).await {
                Ok(price) if quoted.is_some_and(|q| q + 0.5 < price) => return Some(Err(notify_error("办公室租金已变动，请重新查询。"))),
                Err(e) => return Some(Err(e)),
                _ => {}
            }
            rent(db, world, corp, station).await.map(|_| Value::None)
        }
        // Give the office up; its hangars must be empty first.
        "UnrentOffice" => {
            match may_rent(db, me, corp).await {
                Ok(true) => {}
                Ok(false) => return Some(Err(notify_error("你没有退租办公室的职权。"))),
                Err(e) => return Some(Err(e)),
            }
            let office = match office_at(db, corp, station).await {
                Ok(Some(o)) => o,
                Ok(None) => return Some(Ok(Value::None)),
                Err(e) => return Some(Err(e)),
            };
            match db.client.query_opt("SELECT 1 FROM game.items WHERE location_id = $1 LIMIT 1", &[&office]).await {
                Ok(Some(_)) => return Some(Err(notify_error("请先清空军团机库再退租办公室。"))),
                Err(e) => return Some(Err(db_err(e))),
                Ok(None) => {}
            }
            let removed = async {
                db.client.execute("DELETE FROM game.offices WHERE office_id = $1", &[&office]).await.map_err(db_err)?;
                db.client.execute("DELETE FROM game.items WHERE item_id = $1", &[&office]).await.map_err(db_err)?;
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
