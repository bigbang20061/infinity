//! `config` service: the server side of the client's cfg Recordsets
//! (eve/common/script/sys/eveCfg.py). Multi-fetchers return `(header, rows)`.
//! Owner, location, ticker and alliance records come from game and SDE tables.
//! `GetStationSolarSystemsByOwner` returns distinct systems from `sde.sta_stations`;
//! player-owned structures are not included.

use super::{db_err, value_ids, Call, CallError, CallResult};
use crate::client::{short, Client};
use crate::consts::TYPE_CHARACTER;
use marshal::Value;
use tracing::warn;

/// SDE invTypes: Corporation.
const TYPE_CORPORATION: i64 = 2;
/// SDE invTypes: Faction.
const TYPE_FACTION: i64 = 30;
/// SDE invTypes: Alliance.
const TYPE_ALLIANCE: i64 = 16159;
/// Client cfg ticker row: remaining optional fields after owner ID and ticker.
const TICKER_OPTIONAL_FIELDS: usize = 6;

struct StationSystem { solar_system_id: i64 }
impl StationSystem { fn from_row(r: &tokio_postgres::Row) -> Self { Self { solar_system_id: r.get("solar_system_id") } } }


fn header(cols: &[&str]) -> Value {
    Value::list(cols.iter().map(|c| Value::str(*c)))
}

pub async fn owners(c: &Client, ids: &[i64]) -> Result<Value, CallError> {
    let rows = crate::dao::characters::social::owners(&c.db, ids, TYPE_CHARACTER as i64, TYPE_CORPORATION, TYPE_FACTION, TYPE_ALLIANCE).await.map_err(db_err)?;
    let data = rows
        .into_iter()
        .map(|r| {
            Value::list([
                Value::Int(r.id),
                Value::unicode(r.name.unwrap_or_default()),
                Value::Int(r.type_id),
                Value::Int(r.gender.unwrap_or(0) as i64),
                r.name_id.map(Value::Int).unwrap_or(Value::None),
            ])
        })
        .collect();
    Ok(Value::tuple([header(&["ownerID", "ownerName", "typeID", "gender", "ownerNameID"]), Value::List(data)]))
}

async fn locations(c: &Client, ids: &[i64]) -> Result<Value, CallError> {
    // Static map objects resolve locally on the client; the server answers for
    // anything else it knows (stations by name, player items such as ships).
    let rows = crate::dao::characters::social::locations(&c.db, ids).await.map_err(db_err)?;
    let data = rows
        .into_iter()
        .map(|r| {
            Value::list([
                Value::Int(r.id),
                Value::unicode(r.name.unwrap_or_default()),
                r.system.map(Value::Int).unwrap_or(Value::None),
                Value::Float(r.x),
                Value::Float(r.y),
                Value::Float(r.z),
                Value::None,
            ])
        })
        .collect();
    // The client files these into cfg.evelocations positionally with its own descriptor
    // (eveCfg.GetLocationRowDescriptor), so the columns must match it exactly.
    Ok(Value::tuple([header(&["locationID", "locationName", "solarSystemID", "x", "y", "z", "locationNameID"]), Value::List(data)]))
}

async fn tickers(c: &Client, ids: &[i64]) -> Result<Value, CallError> {
    let rows = crate::dao::characters::social::tickers(&c.db, ids).await.map_err(db_err)?;
    let data = rows
        .into_iter()
        .map(|r| {
            let mut row = vec![Value::Int(r.id), Value::str(r.ticker.unwrap_or_default())];
            row.extend(std::iter::repeat_n(Value::None, TICKER_OPTIONAL_FIELDS));
            Value::List(row)
        })
        .collect();
    Ok(Value::tuple([
        header(&["corporationID", "tickerName", "shape1", "shape2", "shape3", "color1", "color2", "color3"]),
        Value::List(data),
    ]))
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> CallResult {
    let keys = value_ids(call.args.first());
    match call.method {
        "GetMultiOwnersEx" | "GetOwnersEx" => owners(c, &keys).await,
        "GetMultiLocationsEx" | "GetLocationsEx" => locations(c, &keys).await,
        "GetMultiCorpTickerNamesEx" | "GetCorpTickerNamesEx" => tickers(c, &keys).await,
        // [KeyVal(solarSystemID)] where the corporation owns a station (info window, map).
        "GetStationSolarSystemsByOwner" => {
            let owner = keys.first().copied().unwrap_or(0);
            let rows = c
                .db
                .query("SELECT DISTINCT solar_system_id::bigint FROM sde.sta_stations WHERE owner_id::bigint = $1", &[&owner])
                .await
                .map_err(db_err)?;
            Ok(Value::list(rows.iter().map(|r| super::keyval([("solarSystemID", Value::Int(StationSystem::from_row(r).solar_system_id))]))))
        }
        "GetMultiAllianceShortNamesEx" | "GetAllianceShortNamesEx" => {
            let rows = crate::dao::characters::social::alliances(&c.db, &keys).await.map_err(db_err)?;
            let data = rows.into_iter().map(|r| Value::list([Value::Int(r.id), Value::unicode(r.name)])).collect();
            Ok(Value::tuple([header(&["allianceID", "shortName"]), Value::List(data)]))
        }
        _ => {
            warn!(
                user = c.login.user_id,
                "UNHANDLED config::{} args={} kwargs={}",
                call.method,
                short(&marshal::Value::Tuple(call.args.to_vec())),
                short(call.kwargs)
            );
            Ok(Value::None)
        }
    }
}
