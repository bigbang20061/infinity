//! Character public information, asset lists and private notes for `charMgr`.
//! Uses `game.characters`, `game.items`, `game.market_orders` and `game.char_notes`
//! through DAOs, and startup SDE station data. Contacts and other unmodelled
//! answers use the fixed-answer table.
use super::*;

async fn public_info(c: &Client, id: i64) -> Result<Value, CallError> {
    let r = crate::dao::characters::social::public_info(&c.db, id, FILETIME_SECOND, FILETIME_UNIX_EPOCH).await.map_err(db_err)?;
    let Some(r) = r else { return Ok(Value::None) };
    let opt = |v: Option<i32>| v.map(|v| Value::Int(v as i64)).unwrap_or(Value::None);
    Ok(keyval([
        ("characterID", Value::Int(id)),
        ("characterName", Value::unicode(r.name)),
        ("typeID", Value::Int(TYPE_CHARACTER as i64)),
        ("corporationID", Value::Int(r.corporation_id)),
        ("allianceID", Value::None),
        ("raceID", Value::Int(r.race_id as i64)),
        ("bloodlineID", Value::Int(r.bloodline_id as i64)),
        ("ancestryID", opt(r.ancestry_id)),
        ("schoolID", opt(r.school_id)),
        ("gender", Value::Int(r.gender_id as i64)),
        ("createDateTime", Value::Int(r.created)),
        ("startDateTime", Value::Int(r.created)),
        ("description", Value::unicode("")),
        ("title", Value::unicode("")),
        ("militiaFactionID", Value::None),
    ]))
}

/// `invInventory(containerGlobal).GetAssetWorth()` -> (assetWorth, plexWorth), valued
/// at the cheapest ask of each type.
async fn asset_worth(c: &Client) -> CallResult {
    let worth = crate::dao::items::social::worth(&c.db, char_id(c)).await.map_err(db_err)?;
    Ok(Value::tuple([Value::Float(worth), Value::Float(0.0)]))
}

/// charactersheet.GetHomeStationRow(): KeyVal(stationID, solarSystemID, stationTypeID);
/// the map reads `.stationID` before checking it, so no home station is a None stationID.
async fn home_station_row(c: &Client, me: i64) -> CallResult {
    let id = crate::dao::characters::social::home_station_id(&c.db, me).await.map_err(db_err)?;
    let Some(r) = id.and_then(|id| c.sde.map.station(id)) else {
        return Ok(keyval([
            ("stationID", Value::None),
            ("solarSystemID", Value::None),
            ("stationTypeID", Value::None),
        ]));
    };
    let solar_system_id = r.solar_system_id.ok_or_else(|| CallError::Internal(anyhow::anyhow!("home station solar_system_id is null")))?;
    let type_id = r.type_id.ok_or_else(|| CallError::Internal(anyhow::anyhow!("home station type_id is null")))?;
    Ok(keyval([
        ("stationID", Value::Int(r.station_id)),
        ("solarSystemID", Value::Int(solar_system_id)),
        ("stationTypeID", Value::Int(type_id)),
    ]))
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let me = char_id(c);
    Some(match (call.service, call.method) {
        ("charMgr", "GetPublicInfo") => {
            let id = call.args.first().and_then(|v| v.as_int()).unwrap_or(me);
            public_info(c, id).await
        }
        // A one-row list (characterInfoWindow reads [0].description, .militiaFactionID).
        ("charMgr", "GetPublicInfo3") => {
            let id = call.args.first().and_then(|v| v.as_int()).unwrap_or(me);
            public_info(c, id).await.map(|info| if info.is_none() { info } else { Value::list([info]) })
        }
        ("charMgr", "GetAssetWorth") => asset_worth(c).await,

        ("charMgr", "GetHomeStationRow") => home_station_row(c, me).await,
        // Assets window (invCache global inventory): stations holding the character's items...
        ("charMgr", "ListStations") => match crate::dao::items::social::stations(&c.db, me).await {
            Ok(rows) => rows.iter().filter_map(|r| c.sde.map.station(r.location_id).map(|station| {
                let type_id = station.type_id.ok_or_else(|| CallError::Internal(anyhow::anyhow!("station {} has no type", r.location_id)))?;
                let system_id = station.solar_system_id.ok_or_else(|| CallError::Internal(anyhow::anyhow!("station {} has no solar system", r.location_id)))?;
                Ok(vec![Value::Int(r.location_id), Value::Int(r.count), Value::Int(type_id), Value::Int(system_id)])
            })).collect::<Result<Vec<_>, CallError>>().map(|rows| rowset(
                &["stationID", "itemCount", "typeID", "solarSystemID"], rows,
            )),
            Err(e) => Err(db_err(e)),
        },
        // ...the items at one of them...
        ("charMgr", "ListStationItems") => {
            let station = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
            crate::services::items::load_items(&c.db, &c.sde, crate::dao::items::loading::ItemQuery::LocationOwner(station, me)).await.map_err(db_err).map(|items| Value::list(items.iter().map(|i| i.row())))
        }
        // ...and everything the character owns, inside containers and ships too (asset search).
        ("charMgr", "ListIncludingContainers") => {
            crate::services::items::load_items(&c.db, &c.sde, crate::dao::items::loading::ItemQuery::OwnerExceptSelf(me)).await.map_err(db_err).map(|items| Value::list(items.iter().map(|i| i.row())))
        }
        // The info window's private notes are scoped to this character.
        ("charMgr", "GetNote") => {
            let item = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
            match crate::dao::characters::social::note(&c.db, me, item).await {
                Ok(r) => Ok(r.map(Value::unicode).unwrap_or(Value::None)),
                Err(e) => Err(db_err(e)),
            }
        }
        ("charMgr", "SetNote") => {
            let item = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
            let note = call.args.get(1).and_then(|v| v.as_text()).map(|t| t.into_owned()).unwrap_or_default();
            let r = if note.trim().is_empty() {
                crate::dao::characters::social::delete_note(&c.db, me, item).await
            } else {
                crate::dao::characters::social::set_note(&c.db, me, item, &note).await
            };
            r.map(|_| Value::None).map_err(db_err)
        }
        _ => return None,
    })
}
