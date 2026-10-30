//! Ships in the hangar and their fittings: boarding, assembling and repackaging
//! (the `ship` station access and `repackagingSvc`), saved fittings
//! (`charFittingMgr`, eve/client/script/environment/fittingSvc.py) and the
//! character's blueprints (`blueprintManager`, eve/client/script/industry/blueprintSvc.py).
//! Fitting modules themselves is inventory moves into slot flags (inventory.rs) plus
//! the online state dogma.rs keeps.
//! Assembly updates packaged source hulls atomically before creating assembled hulls.
//! Assembly follows menucheckers/itemCheckers.py:93-115 and
//! eve/client/script/ui/services/menuSvcExtras/invItemFunctions.py:175-177:
//! current personal station hangar or active ship cargo in space, excluding modular ships in space.
//! Repackaging follows invItemFunctions.py:544-580 and itemCheckers.py:688-705,953-957:
//! direct personal hangars at named SDE stations, including remote stations.
//! Corporation, array and player-structure access is not modeled.

use super::dogma::{ship_state, wire_quantity};
use crate::dao::items::loading::{load_items, ItemQuery};
use super::inventory::notify_item_change;
use super::{db_err, keyval, user_error, value_field, value_id_list, value_ids, Call, CallError, CallResult};
use crate::client::Client;
use crate::consts::{BLUEPRINT_INFINITE_RUNS, CATEGORY_SHIP, FLAG_CARGO, FLAG_HANGAR, INDUSTRY_JOB_STATUS_INSTALLED, IX_LOCATION_ID, IX_SINGLETON, IX_STACK_SIZE};
use crate::db::outbox::{dispatch_client, Outbox};
use crate::messages;
use marshal::Value;
use std::collections::BTreeMap;
use tracing::info;

/// SDE invCategories: Blueprint.
const CATEGORY_BLUEPRINT: i64 = 9;
/// SDE invGroups: Strategic Cruiser (inventorycommon/util.py:126-127).
const GROUP_STRATEGIC_CRUISER: i64 = 963;
/// Server response limit advertised by blueprintManager.GetLimits.
const MAX_BLUEPRINT_RESULTS: i64 = 10_000;

/// Board(shipID, oldShipID) -> shipState: make an assembled ship in this station's hangar
/// the active one (clientDogmaLocation.MakeShipActive).
async fn board(c: &mut Client, ship_id: i64) -> CallResult {
    let char_id = c.session.int("charid").unwrap_or(0);
    let Some(station) = c.session.int("stationid") else { return Err(user_error(messages::MSG_CANT_BOARD_SHIP_WHILE_IN_SPACE, vec![])) };
    let ship = load_items(&c.db, ItemQuery::Id(ship_id)).await.map_err(db_err)?.into_iter().next().ok_or_else(|| user_error(messages::MSG_SHIP_NOT_FOUND, vec![]))?;
    if ship.category_id != CATEGORY_SHIP as i64 || ship.owner_id != char_id || ship.location_id != station {
        return Err(user_error(messages::MSG_CANT_BOARD_SHIP_NOT_IN_HANGAR, vec![]));
    }
    if !ship.singleton {
        return Err(user_error(messages::MSG_CANT_BOARD_UNASSEMBLED_SHIP, vec![]));
    }
    c.db
        .execute("UPDATE game.characters SET ship_id = $2 WHERE character_id = $1", &[&char_id, &ship_id])
        .await
        .map_err(db_err)?;
    info!(user = c.login.user_id, "character {char_id} boarded ship {ship_id}");
    c.change_session(vec![("shipid", Value::Int(ship_id))]);
    ship_state(c, ship_id).await
}

/// Plan one request against a packaged stack; zero in `remaining` marks conversion.
fn assembly_step(original: i32, remaining: Option<i32>) -> Option<(i32, bool, i32)> {
    if remaining == Some(0) { return None; }
    let quantity = remaining.unwrap_or(original);
    if quantity > 1 {
        Some((quantity - 1, true, quantity))
    } else {
        Some((0, false, quantity))
    }
}

fn assembly_source_change(original: i32, remaining: i32) -> Vec<(i64, Value)> {
    let mut change = vec![(IX_STACK_SIZE, Value::Int(original as i64))];
    if remaining == 0 {
        change.push((IX_SINGLETON, Value::Bool(false)));
    }
    change
}

/// AssembleShip(itemIDs, name=, subSystems=) -> the assembled ship's itemID (or IDs).
async fn assemble(c: &mut Client, call: &Call<'_>) -> CallResult {
    let char_id = c.session.int("charid").unwrap_or(0);
    let station = c.session.int("stationid");
    let active_ship = c.session.int("shipid");
    let ids = value_ids(call.args.first());
    let name = call.kwargs.get("name").and_then(|v| v.as_text()).map(|t| t.into_owned());
    let db = c.db.clone();
    let assembled = db.transaction(
        move |tx, outbox: &mut Outbox| {
            let (ids, name) = (ids.clone(), name.clone());
            Box::pin(async move {
                // Simulate requests in their original order, but write each source once
                // in primary-key order before inserting any generated hulls.
                let mut sources = BTreeMap::new();
                let mut actions = Vec::new();
                for id in ids {
                    if !sources.contains_key(&id) {
                        let item = load_items(tx, ItemQuery::Id(id)).await.map_err(db_err)?.into_iter().next()
                            .ok_or_else(|| super::notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
                        let valid_source = if let Some(station) = station {
                            item.location_id == station && item.flag_id == FLAG_HANGAR
                        } else if Some(item.location_id) == active_ship && item.flag_id == FLAG_CARGO {
                            let carrier = load_items(tx, ItemQuery::Id(item.location_id)).await.map_err(db_err)?.into_iter().next();
                            carrier.is_some_and(|carrier| carrier.owner_id == char_id && carrier.category_id == CATEGORY_SHIP as i64)
                        } else {
                            false
                        };
                        if item.owner_id != char_id || !valid_source {
                            return Err(super::notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
                        }
                        if station.is_none() && item.group_id == GROUP_STRATEGIC_CRUISER && !item.singleton {
                            return Err(user_error(messages::MSG_CANT_ASSEMBLE_MODULAR_SHIP_IN_SPACE, vec![]));
                        }
                        sources.insert(id, (item, None::<i32>));
                    }
                    let Some((item, remaining)) = sources.get_mut(&id) else { continue };
                    if item.category_id != CATEGORY_SHIP as i64 || item.singleton {
                        continue;
                    }
                    if let Some((next, split, _)) = assembly_step(item.quantity, *remaining) {
                        *remaining = Some(next);
                        actions.push((id, split));
                    }
                }
                for (id, (item, remaining)) in &sources {
                    let Some(remaining) = remaining else { continue };
                    let changed = if *remaining == 0 {
                        tx.execute("UPDATE game.items SET quantity = CASE WHEN quantity > 1 THEN 1 ELSE quantity END, singleton = true, name = COALESCE($7, name) WHERE item_id = $1 AND owner_id = $2 AND location_id = $3 AND flag_id = $4 AND type_id = $5 AND quantity = $6 AND NOT singleton",
                            &[id, &char_id, &item.location_id, &item.flag_id, &item.type_id, &item.quantity, &name]).await.map_err(db_err)?
                    } else {
                        tx.execute("UPDATE game.items SET quantity = $7 WHERE item_id = $1 AND owner_id = $2 AND location_id = $3 AND flag_id = $4 AND type_id = $5 AND quantity = $6 AND quantity > 1 AND NOT singleton",
                            &[id, &char_id, &item.location_id, &item.flag_id, &item.type_id, &item.quantity, remaining]).await.map_err(db_err)?
                    };
                    if changed == 0 {
                        return Err(super::notify_error(messages::INSUFFICIENT_ITEM_QUANTITY));
                    }
                }
                let mut assembled = Vec::new();
                let mut notified = std::collections::BTreeSet::new();
                for (id, split) in actions {
                    let (item, remaining) = &sources[&id];
                    if notified.insert(id) {
                        notify_item_change(tx, outbox, id, assembly_source_change(item.quantity, remaining.unwrap())).await?;
                    }
                    let ship_id = if split {
                        let new_id: i64 = tx.query_one(
                            "INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton, name)
                             VALUES ($1, $2, $3, $4, 1, true, $5) RETURNING item_id",
                            &[&item.type_id, &char_id, &item.location_id, &item.flag_id, &name],
                        ).await.map_err(db_err)?.get(0);
                        notify_item_change(tx, outbox, new_id, vec![(IX_LOCATION_ID, Value::Int(0))]).await?;
                        new_id
                    } else {
                        id
                    };
                    assembled.push(ship_id);
                }
                Ok(assembled)
            })
        },
        |assembled, outbox| async move { dispatch_client(assembled, outbox, c) },
    ).await?;
    Ok(match assembled.as_slice() {
        [one] => Value::Int(*one),
        _ => Value::list(assembled.into_iter().map(Value::Int)),
    })
}

/// RepackageItems({stationID: [(itemID, locationID)]}, destroyable=): unassemble ships and modules.
async fn repackage(c: &mut Client, call: &Call<'_>) -> CallResult {
    let char_id = c.session.int("charid").unwrap_or(0);
    let active_ship = c.session.int("shipid");
    let mut ids = vec![];
    if let Some(Value::Dict(by_station)) = call.args.first() {
        for (named_station, items) in by_station {
            let named_station = named_station.as_int().ok_or_else(|| super::notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
            for pair in items.items().unwrap_or_default() {
                let pair = pair.items().unwrap_or_default();
                let id = pair.first().and_then(|v| v.as_int()).ok_or_else(|| super::notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
                let location = pair.get(1).and_then(|v| v.as_int()).ok_or_else(|| super::notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
                ids.push((id, location, named_station));
            }
        }
    }
    let mut planned = std::collections::BTreeMap::new();
    for (id, location, named_station) in ids {
        if Some(id) == active_ship {
            return Err(user_error(messages::MSG_CANT_REPACKAGE_ACTIVE_SHIP, vec![]));
        }
        let item = load_items(&c.db, ItemQuery::Id(id)).await.map_err(db_err)?.into_iter().next()
            .ok_or_else(|| super::notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
        if item.owner_id != char_id || item.location_id != named_station || item.location_id != location || item.flag_id != FLAG_HANGAR
            || c.db.query_opt("SELECT 1 FROM sde.sta_stations WHERE station_id = $1::bigint", &[&named_station]).await.map_err(db_err)?.is_none() {
            return Err(super::notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
        }
        if planned.contains_key(&id) {
            continue;
        }
        if !item.singleton {
            continue;
        }
        if item.category_id == CATEGORY_SHIP as i64 {
            let fitted: i64 = c.db.query_one("SELECT count(*) FROM game.items WHERE location_id = $1", &[&id])
                .await.map_err(db_err)?.get(0);
            if fitted > 0 {
                return Err(CallError::User { msg: messages::MSG_CONFIRM_REPACKAGE_SOMETHING_WITH_UPGRADES.into(), dict: vec![] });
            }
        }
        planned.insert(id, item.location_id);
    }
    for (id, station) in planned {
        let changed = c.db.execute("UPDATE game.items SET singleton = false, online = false, name = NULL WHERE item_id = $1 AND owner_id = $2 AND location_id = $3 AND flag_id = $4 AND singleton", &[&id, &char_id, &station, &FLAG_HANGAR])
            .await.map_err(db_err)?;
        if changed == 0 {
            return Err(super::notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
        }
        let mut outbox = Outbox::default();
        notify_item_change(&c.db, &mut outbox, id, vec![(IX_SINGLETON, Value::Bool(true))]).await?;
        dispatch_client((), outbox, c)?;
    }
    Ok(Value::None)
}

// ---------------------------------------------------------------- saved fittings

fn fitting_value(r: &tokio_postgres::Row) -> (Value, Value) {
    let id: i64 = r.get(0);
    let (types, flags, qty): (Vec<i32>, Vec<i32>, Vec<i32>) = (r.get(5), r.get(6), r.get(7));
    let fit_data = types.iter().zip(&flags).zip(&qty).map(|((t, f), q)| Value::tuple([Value::Int(*t as i64), Value::Int(*f as i64), Value::Int(*q as i64)]));
    let fitting = keyval([
        ("fittingID", Value::Int(id)),
        ("ownerID", Value::Int(r.get(1))),
        ("name", Value::unicode(r.get::<_, String>(2))),
        ("description", Value::unicode(r.get::<_, String>(3))),
        ("shipTypeID", Value::Int(r.get::<_, i32>(4) as i64)),
        ("fitData", Value::list(fit_data)),
    ]);
    (Value::Int(id), fitting)
}

async fn fittings(c: &mut Client, call: &Call<'_>) -> CallResult {
    let char_id = c.session.int("charid").unwrap_or(0);
    let owner = call.args.first().and_then(|v| v.as_int()).unwrap_or(char_id);
    if owner != char_id {
        return Ok(match call.method {
            "GetFittings" => Value::Dict(vec![]),
            _ => Value::None,
        });
    }
    let db = &c.db;
    const COLS: &str = "fitting_id, owner_id, name, description, ship_type_id, fit_types, fit_flags, fit_qty";
    Ok(match call.method {
        // {fittingID: KeyVal(fittingID, ownerID, name, description, shipTypeID, fitData)}
        "GetFittings" => {
            let rows = db.query(&format!("SELECT {COLS} FROM game.fittings WHERE owner_id = $1 ORDER BY fitting_id"), &[&owner]).await.map_err(db_err)?;
            Value::Dict(rows.iter().map(fitting_value).collect())
        }
        // SaveManyFittings(ownerID, {tempID: fitting}) -> [KeyVal(tempFittingID, realFittingID)]
        "SaveManyFittings" => {
            let mut saved = vec![];
            if let Some(Value::Dict(fits)) = call.args.get(1) {
                for (temp, fit) in fits {
                    let text = |k: &str| value_field(fit, k).and_then(|v| v.as_text()).map(|t| t.into_owned()).unwrap_or_default();
                    let ship_type = value_field(fit, "shipTypeID").and_then(|v| v.as_int()).unwrap_or(0) as i32;
                    let (mut types, mut flags, mut qty) = (vec![], vec![], vec![]);
                    for row in value_field(fit, "fitData").and_then(|v| v.items()).unwrap_or_default() {
                        if let Some([t, f, q]) = row.items() {
                            types.push(t.as_int().unwrap_or(0) as i32);
                            flags.push(f.as_int().unwrap_or(0) as i32);
                            qty.push(q.as_int().unwrap_or(1) as i32);
                        }
                    }
                    let id: i64 = db
                        .query_one(
                            "INSERT INTO game.fittings (owner_id, name, description, ship_type_id, fit_types, fit_flags, fit_qty)
                             VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING fitting_id",
                            &[&owner, &text("name"), &text("description"), &ship_type, &types, &flags, &qty],
                        )
                        .await
                        .map_err(db_err)?
                        .get(0);
                    saved.push(keyval([("tempFittingID", temp.clone()), ("realFittingID", Value::Int(id))]));
                }
            }
            Value::List(saved)
        }
        "DeleteFitting" => {
            let id = call.args.get(1).and_then(|v| v.as_int()).unwrap_or(0);
            db.execute("DELETE FROM game.fittings WHERE owner_id = $1 AND fitting_id = $2", &[&owner, &id]).await.map_err(db_err)?;
            Value::None
        }
        "DeleteManyFittings" => {
            let ids = value_id_list(call.args.get(1));
            db.execute("DELETE FROM game.fittings WHERE owner_id = $1 AND fitting_id = ANY($2)", &[&owner, &ids]).await.map_err(db_err)?;
            Value::None
        }
        _ => return Err(CallError::Internal(anyhow::anyhow!("charFittingMgr::{} not handled", call.method))),
    })
}

/// UpdateNameAndDescription(fittingID, ownerID, name, description)
async fn rename_fitting(c: &Client, call: &Call<'_>) -> CallResult {
    let char_id = c.session.int("charid").unwrap_or(0);
    let id = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
    let text = |i: usize| call.args.get(i).and_then(|v| v.as_text()).map(|t| t.into_owned()).unwrap_or_default();
    c.db
        .execute("UPDATE game.fittings SET name = $3, description = $4 WHERE fitting_id = $1 AND owner_id = $2", &[&id, &char_id, &text(2), &text(3)])
        .await
        .map_err(db_err)?;
    Ok(Value::None)
}

// ---------------------------------------------------------------- blueprints

/// Blueprint items of `owner` as industryCommon.BlueprintInstance data. Originals are
/// singletons (quantity -1, runs -1) or stacks carrying their quantity; copies are
/// singletons of quantity -2. Research state comes from game.blueprints, and a blueprint
/// in (a container in) a station has that station as its facility.
async fn blueprints(c: &Client, owner: i64, item: Option<i64>) -> Result<Vec<Value>, CallError> {
    let rows = c
        .db
        .query(
            "SELECT i.item_id, i.type_id, i.owner_id, i.location_id, i.flag_id, i.quantity, i.singleton,
                    COALESCE(loc.type_id, st.type_id), COALESCE(st.solar_system_id::bigint, loc.location_id, ch.solar_system_id),
                    st.station_id::bigint, COALESCE(b.material_efficiency, 0)::bigint, COALESCE(b.time_efficiency, 0)::bigint,
                    COALESCE(b.runs, $4::int)::bigint,
                    (SELECT j.job_id FROM game.industry_jobs j WHERE j.blueprint_id = i.item_id AND j.status = $5::int LIMIT 1)
             FROM game.items i
             JOIN sde.inv_types t USING (type_id)
             LEFT JOIN game.items loc ON loc.item_id = i.location_id
             LEFT JOIN sde.sta_stations st ON st.station_id::bigint = COALESCE(loc.location_id, i.location_id)
             LEFT JOIN game.characters ch ON ch.character_id = i.owner_id
             LEFT JOIN game.blueprints b ON b.item_id = i.item_id
             WHERE i.owner_id = $1 AND t.category_id = $2 AND ($3::bigint IS NULL OR i.item_id = $3) AND i.location_id > 0",
            &[&owner, &(CATEGORY_BLUEPRINT as i32), &item, &BLUEPRINT_INFINITE_RUNS, &INDUSTRY_JOB_STATUS_INSTALLED],
        )
        .await
        .map_err(db_err)?;
    Ok(rows
        .iter()
        .map(|r| {
            let singleton: bool = r.get(6);
            let stored = r.get::<_, i32>(5) as i64;
            let quantity = wire_quantity(singleton, stored);
            keyval([
                ("itemID", Value::Int(r.get(0))),
                ("typeID", Value::Int(r.get::<_, i32>(1) as i64)),
                ("ownerID", Value::Int(r.get(2))),
                ("locationID", Value::Int(r.get(3))),
                ("locationTypeID", r.get::<_, Option<i32>>(7).map(|t| Value::Int(t as i64)).unwrap_or(Value::None)),
                ("locationFlagID", Value::Int(FLAG_HANGAR as i64)),
                ("flagID", Value::Int(r.get::<_, i32>(4) as i64)),
                ("facilityID", r.get::<_, Option<i64>>(9).map(Value::Int).unwrap_or(Value::None)),
                ("solarSystemID", r.get::<_, Option<i64>>(8).map(Value::Int).unwrap_or(Value::None)),
                ("quantity", Value::Int(quantity)),
                ("runs", Value::Int(r.get(12))),
                ("timeEfficiency", Value::Int(r.get(11))),
                ("materialEfficiency", Value::Int(r.get(10))),
                ("jobID", r.get::<_, Option<i64>>(13).map(Value::Int).unwrap_or(Value::None)),
                ("isImpounded", Value::Bool(false)),
            ])
        })
        .collect())
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let char_id = c.session.int("charid").unwrap_or(0);
    Some(match (call.service, call.method) {
        ("ship", "Board") => board(c, call.args.first().and_then(|v| v.as_int()).unwrap_or(0)).await,
        ("ship", "AssembleShip") => assemble(c, call).await,
        ("ship", "CheckEnoughSubSystemToUndock") => Ok(Value::Bool(true)),
        ("repackagingSvc", "RepackageItems") => repackage(c, call).await,
        ("charFittingMgr", "UpdateNameAndDescription") => rename_fitting(c, call).await,
        ("charFittingMgr", _) => fittings(c, call).await,
        ("corpFittingMgr", "GetFittings") | ("allianceFittingMgr", "GetFittings") => Ok(Value::Dict(vec![])),
        // {fittingID: fitting} shared by the community corporation: none.
        ("corpFittingMgr", "GetCommunityFittings") => Ok(Value::Dict(vec![])),

        ("blueprintManager", "GetLimits") => Ok(Value::dict([("maxBlueprintResults", Value::Int(MAX_BLUEPRINT_RESULTS))])),
        // GetBlueprintDataByOwner(ownerID, facilityID) -> (blueprints, {facilityID: count})
        ("blueprintManager", "GetBlueprintDataByOwner") => {
            let owner = call.args.first().and_then(|v| v.as_int()).unwrap_or(char_id);
            match blueprints(c, owner, None).await {
                Ok(bps) => {
                    let count = bps.len() as i64;
                    Ok(Value::tuple([Value::List(bps), Value::Dict(vec![(Value::None, Value::Int(count))])]))
                }
                Err(e) => Err(e),
            }
        }
        ("blueprintManager", "GetBlueprintData") => {
            let item = call.args.first().and_then(|v| v.as_int());
            match blueprints(c, char_id, item).await {
                Ok(bps) => bps.into_iter().next().ok_or_else(|| user_error(messages::MSG_BLUEPRINT_NOT_FOUND, vec![])),
                Err(e) => Err(e),
            }
        }
        _ => return None,
    })
}

#[cfg(test)] mod tests;
