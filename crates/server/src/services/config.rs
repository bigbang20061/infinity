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


fn header(cols: &[&str]) -> Value {
    Value::list(cols.iter().map(|c| Value::str(*c)))
}

pub async fn owners(c: &Client, ids: &[i64]) -> Result<Value, CallError> {
    let rows = c
        .db
        .query(
            "SELECT character_id, name, $2::bigint, gender_id, NULL::bigint FROM game.characters WHERE character_id = ANY($1::bigint[])
             UNION ALL SELECT corporation_id, name, $3::bigint, 0, NULL FROM sde.crp_npc_corporations WHERE corporation_id = ANY($1::bigint[])
             UNION ALL SELECT corporation_id, name, $3::bigint, 0, NULL FROM game.corporations WHERE corporation_id = ANY($1::bigint[])
             UNION ALL SELECT character_id, data->'name'->>'en', $2::bigint, (data->>'gender')::boolean::int, (data->>'nameID')::bigint
                       FROM sde.npc_characters WHERE character_id = ANY($1::bigint[])
             UNION ALL SELECT faction_id, name, $4::bigint, 0, NULL FROM sde.chr_factions WHERE faction_id = ANY($1::bigint[])
             UNION ALL SELECT alliance_id, name, $5::bigint, 0, NULL FROM game.alliances WHERE alliance_id = ANY($1::bigint[])",
            &[&ids, &(TYPE_CHARACTER as i64), &TYPE_CORPORATION, &TYPE_FACTION, &TYPE_ALLIANCE],
        )
        .await
        .map_err(db_err)?;
    let data = rows
        .iter()
        .map(|r| {
            Value::list([
                Value::Int(r.get::<_, i64>(0)),
                Value::unicode(r.get::<_, Option<String>>(1).unwrap_or_default()),
                Value::Int(r.get(2)),
                Value::Int(r.get::<_, Option<i32>>(3).unwrap_or(0) as i64),
                r.get::<_, Option<i64>>(4).map(Value::Int).unwrap_or(Value::None),
            ])
        })
        .collect();
    Ok(Value::tuple([header(&["ownerID", "ownerName", "typeID", "gender", "ownerNameID"]), Value::List(data)]))
}

async fn locations(c: &Client, ids: &[i64]) -> Result<Value, CallError> {
    // Static map objects resolve locally on the client; the server answers for
    // anything else it knows (stations by name, player items such as ships).
    let rows = c
        .db
        .query(
            "SELECT item_id, COALESCE(i.name, t.name), NULL::bigint, 0::float8, 0::float8, 0::float8 FROM game.items i
                 LEFT JOIN sde.inv_types t USING (type_id) WHERE item_id = ANY($1::bigint[])
             UNION ALL SELECT solar_system_id::bigint, name, solar_system_id::bigint, x, y, z FROM sde.map_solar_systems WHERE solar_system_id = ANY($1::bigint[])",
            &[&ids],
        )
        .await
        .map_err(db_err)?;
    let data = rows
        .iter()
        .map(|r| {
            Value::list([
                Value::Int(r.get(0)),
                Value::unicode(r.get::<_, Option<String>>(1).unwrap_or_default()),
                r.get::<_, Option<i64>>(2).map(Value::Int).unwrap_or(Value::None),
                Value::Float(r.get(3)),
                Value::Float(r.get(4)),
                Value::Float(r.get(5)),
                Value::None,
            ])
        })
        .collect();
    // The client files these into cfg.evelocations positionally with its own descriptor
    // (eveCfg.GetLocationRowDescriptor), so the columns must match it exactly.
    Ok(Value::tuple([header(&["locationID", "locationName", "solarSystemID", "x", "y", "z", "locationNameID"]), Value::List(data)]))
}

async fn tickers(c: &Client, ids: &[i64]) -> Result<Value, CallError> {
    let rows = c
        .db
        .query(
            "SELECT corporation_id::bigint, ticker FROM sde.crp_npc_corporations WHERE corporation_id = ANY($1::bigint[])
             UNION ALL SELECT corporation_id, ticker FROM game.corporations WHERE corporation_id = ANY($1::bigint[])",
            &[&ids],
        )
        .await
        .map_err(db_err)?;
    let data = rows
        .iter()
        .map(|r| {
            let mut row = vec![Value::Int(r.get(0)), Value::str(r.get::<_, Option<String>>(1).unwrap_or_default())];
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
            Ok(Value::list(rows.iter().map(|r| super::keyval([("solarSystemID", Value::Int(r.get(0)))]))))
        }
        "GetMultiAllianceShortNamesEx" | "GetAllianceShortNamesEx" => {
            let rows = c
                .db
                .query("SELECT alliance_id, short_name FROM game.alliances WHERE alliance_id = ANY($1::bigint[])", &[&keys])
                .await
                .map_err(db_err)?;
            let data = rows.iter().map(|r| Value::list([Value::Int(r.get(0)), Value::unicode(r.get::<_, String>(1))])).collect();
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
