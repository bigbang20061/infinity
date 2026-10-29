//! Character-scoped services once in game: charMgr, account, station guests,
//! online status and the skill handler.

use super::{db_err, keyval, rowset, Call, CallError, CallResult};
use crate::client::Client;
use crate::consts::{FILETIME_SECOND, FILETIME_UNIX_EPOCH, TYPE_CHARACTER};
use marshal::Value;

/// Client paperdoll.State.no_recustomization protocol state.
const PAPERDOLL_NO_RECUSTOMIZATION: i64 = 0;

fn char_id(c: &Client) -> i64 {
    c.session.int("charid").unwrap_or(0)
}

async fn public_info(c: &Client, id: i64) -> Result<Value, CallError> {
    let r = c
        .db
        .client
        .query_opt(
            "SELECT name, corporation_id, race_id, bloodline_id, ancestry_id, school_id, gender_id,
                    (extract(epoch FROM created_at) * $2::bigint)::bigint + $3::bigint
             FROM game.characters WHERE character_id = $1",
            &[&id, &FILETIME_SECOND, &FILETIME_UNIX_EPOCH],
        )
        .await
        .map_err(db_err)?;
    let Some(r) = r else { return Ok(Value::None) };
    let opt = |v: Option<i32>| v.map(|v| Value::Int(v as i64)).unwrap_or(Value::None);
    Ok(keyval([
        ("characterID", Value::Int(id)),
        ("characterName", Value::unicode(r.get::<_, String>(0))),
        ("typeID", Value::Int(TYPE_CHARACTER as i64)),
        ("corporationID", Value::Int(r.get(1))),
        ("allianceID", Value::None),
        ("raceID", Value::Int(r.get::<_, i32>(2) as i64)),
        ("bloodlineID", Value::Int(r.get::<_, i32>(3) as i64)),
        ("ancestryID", opt(r.get(4))),
        ("schoolID", opt(r.get(5))),
        ("gender", Value::Int(r.get::<_, i32>(6) as i64)),
        ("createDateTime", Value::Int(r.get(7))),
        ("startDateTime", Value::Int(r.get(7))),
        ("description", Value::unicode("")),
        ("title", Value::unicode("")),
        ("militiaFactionID", Value::None),
    ]))
}

async fn balance(c: &Client) -> Result<f64, CallError> {
    Ok(c
        .db
        .client
        .query_one("SELECT balance::float8 FROM game.characters WHERE character_id = $1", &[&char_id(c)])
        .await
        .map_err(db_err)?
        .get(0))
}

/// A character's stored paperdoll blob (`appearance` or `portrait`), as the client sent it.
async fn doll_blob(c: &Client, id: i64, column: &str) -> Result<Value, CallError> {
    let row = c
        .db
        .client
        .query_opt(&format!("SELECT {column} FROM game.characters WHERE character_id = $1"), &[&id])
        .await
        .map_err(db_err)?;
    match row.and_then(|r| r.get::<_, Option<Vec<u8>>>(0)) {
        Some(blob) => marshal::loads(&blob).map_err(|e| CallError::Internal(anyhow::anyhow!("{column} of {id}: {e:?}"))),
        None => Ok(Value::None),
    }
}

/// The character creator leaves unset sculpt weights and resource variations as None;
/// the client reads them like database columns (`None < 0.0` then `abs(None)` in
/// character.ConvertWeightKeyValue), so serve them as zeros.
fn zero_doll_nulls(v: &mut Value) {
    match v {
        Value::Instance(_, state) => zero_doll_nulls(state),
        Value::Dict(pairs) => pairs.iter_mut().for_each(|(_, v)| zero_doll_nulls(v)),
        Value::List(items) => items.iter_mut().for_each(zero_doll_nulls),
        Value::NewObj { data, .. } => {
            if let Value::Tuple(outer) = data.as_mut() {
                if let Some(Value::Tuple(args)) = outer.first_mut() {
                    let float = matches!(args.first(), Some(Value::Global(c)) if c.ends_with("SculptingRow"));
                    for a in args.iter_mut().skip(1).filter(|a| a.is_none()) {
                        *a = if float { Value::Float(0.0) } else { Value::Int(0) };
                    }
                }
            }
        }
        _ => {}
    }
}

/// `invInventory(containerGlobal).GetAssetWorth()` -> (assetWorth, plexWorth), valued
/// at the cheapest ask of each type.
async fn asset_worth(c: &Client) -> CallResult {
    let r = c
        .db
        .client
        .query_one(
            "SELECT COALESCE(sum(GREATEST(i.quantity, 1) *
                    (SELECT min(o.price) FROM game.market_orders o WHERE o.type_id = i.type_id AND NOT o.bid)), 0)::float8
             FROM game.items i WHERE i.owner_id = $1",
            &[&char_id(c)],
        )
        .await
        .map_err(db_err)?;
    Ok(Value::tuple([Value::Float(r.get(0)), Value::Float(0.0)]))
}

/// paperDollServer.UpdateExistingCharacter{Full,Limited}(charID, dollInfo, portraitInfo, dollExists)
async fn store_doll(c: &Client, call: &Call<'_>) -> CallResult {
    let id = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
    let appearance = marshal::dumps(call.args.get(1).unwrap_or(&Value::None));
    let portrait = marshal::dumps(call.args.get(2).unwrap_or(&Value::None));
    c.db.client
        .execute(
            "UPDATE game.characters SET appearance = $3, portrait = $4 WHERE character_id = $1 AND user_id = $2",
            &[&id, &c.login.user_id, &appearance, &portrait],
        )
        .await
        .map_err(db_err)?;
    Ok(Value::None)
}

/// charactersheet.GetHomeStationRow(): KeyVal(stationID, solarSystemID, stationTypeID);
/// the map reads `.stationID` before checking it, so no home station is a None stationID.
async fn home_station_row(c: &Client, me: i64) -> CallResult {
    let r = c
        .db
        .client
        .query_opt(
            "SELECT s.station_id::bigint, s.solar_system_id::bigint, s.type_id::bigint FROM game.characters ch
             JOIN sde.sta_stations s ON s.station_id = ch.home_station_id WHERE ch.character_id = $1",
            &[&me],
        )
        .await
        .map_err(db_err)?;
    let col = |i: usize| r.as_ref().map(|r| Value::Int(r.get(i))).unwrap_or(Value::None);
    Ok(keyval([("stationID", col(0)), ("solarSystemID", col(1)), ("stationTypeID", col(2))]))
}

/// Employment history: the current corporation since the character was created.
async fn employment(c: &Client, id: i64) -> CallResult {
    let history = super::corps::employment_rows(&c.db, id).await?;
    Ok(Value::tuple([Value::List(history), Value::list([])]))
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
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
        // ---- paperdoll ----
        ("charMgr", "GetPaperdollState") => Ok(Value::Int(PAPERDOLL_NO_RECUSTOMIZATION)),
        ("paperDollServer", "GetMyPaperDollData") | ("paperDollServer", "GetPaperDollData") => {
            let id = call.args.first().and_then(|v| v.as_int()).unwrap_or(me);
            doll_blob(c, id, "appearance").await.map(|mut v| {
                zero_doll_nulls(&mut v);
                v
            })
        }
        // (portraits, portraitData): ccSvc reads portraits[0].
        ("paperDollServer", "GetPaperDollPortraitDataFor") => {
            let id = call.args.first().and_then(|v| v.as_int()).unwrap_or(me);
            doll_blob(c, id, "portrait").await.map(|p| {
                let portraits = if p.is_none() { vec![] } else { vec![p] };
                Value::tuple([Value::List(portraits), Value::None])
            })
        }
        ("paperDollServer", "UpdateExistingCharacterFull") | ("paperDollServer", "UpdateExistingCharacterLimited") => {
            store_doll(c, call).await
        }
        ("charMgr", "SetActivityStatus") => Ok(Value::None),
        ("charMgr", "GetAssetWorth") => asset_worth(c).await,

        ("charMgr", "GetCharacterDescription") => Ok(Value::unicode("")),
        ("charMgr", "GetHomeStationRow") => home_station_row(c, me).await,
        // (history [row(corporationID, deleted, startDate)], transfers)
        ("corporationSvc", "GetEmployementRecordAndCharacterTransfers") => {
            let id = call.args.first().and_then(|v| v.as_int()).unwrap_or(me);
            employment(c, id).await
        }
        // Just the history, newest first (facWarSvc.IsCharacterInFwCooldown).
        ("corporationSvc", "GetEmploymentRecord") => {
            let id = call.args.first().and_then(|v| v.as_int()).unwrap_or(me);
            super::corps::employment_rows(&c.db, id).await.map(Value::List)
        }

        ("charMgr", "GetContactList") => Ok(keyval([
            ("addresses", rowset(&["contactID", "inWatchlist", "relationshipID", "labelMask"], vec![])),
            ("blocked", rowset(&["senderID"], vec![])),
        ])),
        // Assets window (invCache global inventory): stations holding the character's items...
        ("charMgr", "ListStations") => match c
            .db
            .client
            .query(
                "SELECT i.location_id, count(*), st.type_id::bigint, st.solar_system_id::bigint FROM game.items i
                 JOIN sde.sta_stations st ON st.station_id = i.location_id
                 WHERE i.owner_id = $1 GROUP BY i.location_id, st.type_id, st.solar_system_id",
                &[&me],
            )
            .await
        {
            Ok(rows) => Ok(rowset(
                &["stationID", "itemCount", "typeID", "solarSystemID"],
                rows.iter().map(|r| vec![Value::Int(r.get(0)), Value::Int(r.get(1)), Value::Int(r.get(2)), Value::Int(r.get(3))]).collect(),
            )),
            Err(e) => Err(db_err(e)),
        },
        // ...the items at one of them...
        ("charMgr", "ListStationItems") => {
            let station = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
            super::dogma::load_items(c, "i.location_id = $1 AND i.owner_id = $2", &[&station, &me]).await.map(|items| Value::list(items.iter().map(|i| i.row())))
        }
        // ...and everything the character owns, inside containers and ships too (asset search).
        ("charMgr", "ListIncludingContainers") => {
            super::dogma::load_items(c, "i.owner_id = $1 AND i.item_id <> $1", &[&me]).await.map(|items| Value::list(items.iter().map(|i| i.row())))
        }
        // GetRepairQuotes(itemIDs) -> {itemID: [KeyVal(itemID, typeID, damage, maxHealth,
        // costToRepairOneUnitOfDamage)]}. Docking already repairs ships (combat::repair), so
        // nothing in a station is damaged.
        ("repairSvc", "GetRepairQuotes") => {
            let ids: Vec<i64> = call.args.first().and_then(|v| v.items()).unwrap_or(&[]).iter().filter_map(|v| v.as_int()).collect();
            match c
                .db
                .client
                .query(
                    "SELECT i.item_id, i.type_id::bigint, COALESCE((SELECT value FROM sde.type_attributes a WHERE a.type_id = i.type_id AND a.attribute_id = 9), 0)::float8
                     FROM game.items i WHERE i.item_id = ANY($1) AND i.owner_id = $2",
                    &[&ids, &me],
                )
                .await
            {
                Ok(rows) => Ok(Value::Dict(
                    rows.iter()
                        .map(|r| {
                            let id: i64 = r.get(0);
                            let quote = keyval([
                                ("itemID", Value::Int(id)),
                                ("typeID", Value::Int(r.get(1))),
                                ("damage", Value::Float(0.0)),
                                ("maxHealth", Value::Float(r.get(2))),
                                ("costToRepairOneUnitOfDamage", Value::Float(0.0)),
                            ]);
                            (Value::Int(id), Value::list([quote]))
                        })
                        .collect(),
                )),
                Err(e) => Err(db_err(e)),
            }
        }
        ("repairSvc", "RepairItemsInStation") | ("repairSvc", "RepairItemsInStructure") => Ok(Value::None),
        // The info window's private notes: GetNote(itemID) -> text or None, SetNote(itemID, text).
        ("charMgr", "GetNote") => {
            let item = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
            match c.db.client.query_opt("SELECT note FROM game.char_notes WHERE char_id = $1 AND item_id = $2", &[&me, &item]).await {
                Ok(r) => Ok(r.map(|r| Value::unicode(r.get::<_, String>(0))).unwrap_or(Value::None)),
                Err(e) => Err(db_err(e)),
            }
        }
        ("charMgr", "SetNote") => {
            let item = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
            let note = call.args.get(1).and_then(|v| v.as_text()).map(|t| t.into_owned()).unwrap_or_default();
            let r = if note.trim().is_empty() {
                c.db.client.execute("DELETE FROM game.char_notes WHERE char_id = $1 AND item_id = $2", &[&me, &item]).await
            } else {
                c.db.client
                    .execute(
                        "INSERT INTO game.char_notes (char_id, item_id, note) VALUES ($1, $2, $3)
                         ON CONFLICT (char_id, item_id) DO UPDATE SET note = $3",
                        &[&me, &item, &note],
                    )
                    .await
            };
            r.map(|_| Value::None).map_err(db_err)
        }
        // Nothing is held in asset safety: there are no player structures.
        ("structureAssetSafety", "GetItemsInSafetyForCharacter") | ("structureAssetSafety", "GetItemsInSafetyForCorp") => Ok(Value::list([])),
        // The server enforces no banned words of its own; the client checks its regexes.
        ("bannedwords", "check_words_allowed") => Ok(Value::None),
        // KeyVal(id, type_id, solar_system_id, is_fallback)
        ("home_station", "get_home_station") => {
            let r = c
                .db
                .client
                .query_opt(
                    "SELECT s.station_id::bigint, s.type_id::bigint, s.solar_system_id::bigint FROM game.characters ch
                     JOIN sde.sta_stations s ON s.station_id = ch.home_station_id WHERE ch.character_id = $1",
                    &[&me],
                )
                .await
                .map_err(db_err);
            r.map(|r| match r {
                Some(r) => keyval([
                    ("id", Value::Int(r.get(0))),
                    ("type_id", Value::Int(r.get(1))),
                    ("solar_system_id", Value::Int(r.get(2))),
                    ("is_fallback", Value::Bool(false)),
                ]),
                None => Value::None,
            })
        }
        ("account", "GetCashBalance") => {
            let is_corp = call.args.first().and_then(|v| v.as_int()).unwrap_or(0) != 0;
            if is_corp {
                Ok(Value::Float(0.0))
            } else {
                balance(c).await.map(Value::Float)
            }
        }
        ("onlineStatus", "GetInitialState") => Ok(rowset(&["contactID", "online"], vec![])),
        // [(charID, corpID, allianceID, warFactionID)] for everyone docked here.
        ("station", "GetGuests") => {
            let corp = c.session.get("corpid").clone();
            Ok(Value::list([Value::tuple([Value::Int(me), corp, Value::None, Value::None])]))
        }
        // No-op / empty answers for station systems we don't model yet.
        ("corpRegistry", "GetAggressionSettings") => Ok(Value::None),
        ("accessGroupBookmarkMgr", "AddFolder") => Ok(Value::None),
        ("corpStationMgr", "DoStandingCheckForStationService") => Ok(Value::None),
        ("RWManager", "solarsystem_contains_rw_instances") => Ok(Value::Bool(false)),
        ("dynamicBountyMgr", "GetOutputForClientSolarSystem") => Ok(Value::tuple([Value::None, Value::Bool(false)])),
        ("shipAdaptionMgr", "fetch_user_data") => Ok(Value::None),
        _ => return None,
    })
}
