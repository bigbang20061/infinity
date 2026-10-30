//! Ships in the hangar and their fittings: boarding, assembling and repackaging
//! (the `ship` station access and `repackagingSvc`), saved fittings
//! (`charFittingMgr`, eve/client/script/environment/fittingSvc.py) and the
//! character's blueprints (`blueprintManager`, eve/client/script/industry/blueprintSvc.py).
//! Fitting modules themselves is inventory moves into slot flags (inventory.rs) plus
//! the online state dogma.rs keeps.

use super::dogma::{load_items, ship_state, wire_quantity};
use super::inventory::notify_item_change;
use super::{db_err, keyval, user_error, value_field, value_id_list, value_ids, Call, CallError, CallResult};
use crate::client::Client;
use crate::consts::{BLUEPRINT_INFINITE_RUNS, CATEGORY_SHIP, FLAG_HANGAR, INDUSTRY_JOB_STATUS_INSTALLED, IX_LOCATION_ID, IX_SINGLETON, IX_STACK_SIZE};
use crate::db::outbox::{dispatch_client, Outbox};
use marshal::Value;
use tracing::info;

/// SDE invCategories: Blueprint.
const CATEGORY_BLUEPRINT: i64 = 9;
/// Server response limit advertised by blueprintManager.GetLimits.
const MAX_BLUEPRINT_RESULTS: i64 = 10_000;

/// Board(shipID, oldShipID) -> shipState: make an assembled ship in this station's hangar
/// the active one (clientDogmaLocation.MakeShipActive).
async fn board(c: &mut Client, ship_id: i64) -> CallResult {
    let char_id = c.session.int("charid").unwrap_or(0);
    let Some(station) = c.session.int("stationid") else { return Err(user_error("CantBoardShipWhileInSpace", vec![])) };
    let ship = load_items(c, "i.item_id = $1", &[&ship_id]).await?.into_iter().next().ok_or_else(|| user_error("ShipNotFound", vec![]))?;
    if ship.category_id != CATEGORY_SHIP as i64 || ship.owner_id != char_id || ship.location_id != station {
        return Err(user_error("CantBoardShipNotInHangar", vec![]));
    }
    if !ship.singleton {
        return Err(user_error("CantBoardUnassembledShip", vec![]));
    }
    c.db
        .execute("UPDATE game.characters SET ship_id = $2 WHERE character_id = $1", &[&char_id, &ship_id])
        .await
        .map_err(db_err)?;
    info!(user = c.login.user_id, "character {char_id} boarded ship {ship_id}");
    c.change_session(vec![("shipid", Value::Int(ship_id))]);
    ship_state(c, ship_id).await
}

/// AssembleShip(itemIDs, name=, subSystems=) -> the assembled ship's itemID (or IDs).
async fn assemble(c: &mut Client, call: &Call<'_>) -> CallResult {
    let char_id = c.session.int("charid").unwrap_or(0);
    let ids = value_ids(call.args.first());
    let name = call.kwargs.get("name").and_then(|v| v.as_text()).map(|t| t.into_owned());
    let mut assembled = vec![];
    let mut outbox = Outbox::default();
    for id in ids {
        let Some(item) = load_items(c, "i.item_id = $1 AND i.owner_id = $2", &[&id, &char_id]).await?.into_iter().next() else { continue };
        if item.category_id != CATEGORY_SHIP as i64 || item.singleton {
            continue;
        }
        // One hull off a stack becomes the ship; the rest stays packaged.
        let ship_id = if item.quantity > 1 {
            c.db.execute("UPDATE game.items SET quantity = quantity - 1 WHERE item_id = $1", &[&id]).await.map_err(db_err)?;
            notify_item_change(&c.db, &mut outbox, id, vec![(IX_STACK_SIZE, Value::Int(item.quantity as i64))]).await?;
            dispatch_client((), std::mem::take(&mut outbox), c)?;
            let new_id: i64 = c
                .db
                .query_one(
                    "INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton, name)
                     VALUES ($1, $2, $3, $4, 1, true, $5) RETURNING item_id",
                    &[&item.type_id, &char_id, &item.location_id, &item.flag_id, &name],
                )
                .await
                .map_err(db_err)?
                .get(0);
            notify_item_change(&c.db, &mut outbox, new_id, vec![(IX_LOCATION_ID, Value::Int(0))]).await?;
            dispatch_client((), std::mem::take(&mut outbox), c)?;
            new_id
        } else {
            c.db
                .execute("UPDATE game.items SET singleton = true, name = COALESCE($2, name) WHERE item_id = $1", &[&id, &name])
                .await
                .map_err(db_err)?;
            notify_item_change(&c.db, &mut outbox, id, vec![(IX_SINGLETON, Value::Bool(false))]).await?;
            dispatch_client((), std::mem::take(&mut outbox), c)?;
            id
        };
        assembled.push(ship_id);
    }
    Ok(match assembled.as_slice() {
        [one] => Value::Int(*one),
        _ => Value::list(assembled.into_iter().map(Value::Int)),
    })
}

/// RepackageItems({stationID: [(itemID, locationID)]}, destroyable=): unassemble ships and modules.
async fn repackage(c: &mut Client, call: &Call<'_>) -> CallResult {
    let char_id = c.session.int("charid").unwrap_or(0);
    let mut outbox = Outbox::default();
    let mut ids = vec![];
    if let Some(Value::Dict(by_station)) = call.args.first() {
        for (_, items) in by_station {
            for pair in items.items().unwrap_or_default() {
                if let Some(id) = pair.items().and_then(|p| p.first()).and_then(|v| v.as_int()) {
                    ids.push(id);
                }
            }
        }
    }
    for id in ids {
        if Some(id) == c.session.int("shipid") {
            return Err(user_error("CantRepackageActiveShip", vec![]));
        }
        let Some(item) = load_items(c, "i.item_id = $1 AND i.owner_id = $2", &[&id, &char_id]).await?.into_iter().next() else { continue };
        if !item.singleton {
            continue;
        }
        if item.category_id == CATEGORY_SHIP as i64 {
            let fitted: i64 = c
                .db
                .query_one("SELECT count(*) FROM game.items WHERE location_id = $1", &[&id])
                .await
                .map_err(db_err)?
                .get(0);
            if fitted > 0 {
                return Err(CallError::User { msg: "ConfirmRepackageSomethingWithUpgrades".into(), dict: vec![] });
            }
        }
        c.db
            .execute("UPDATE game.items SET singleton = false, online = false, name = NULL WHERE item_id = $1", &[&id])
            .await
            .map_err(db_err)?;
        notify_item_change(&c.db, &mut outbox, id, vec![(IX_SINGLETON, Value::Bool(true))]).await?;
        dispatch_client((), std::mem::take(&mut outbox), c)?;
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
                Ok(bps) => bps.into_iter().next().ok_or_else(|| user_error("BlueprintNotFound", vec![])),
                Err(e) => Err(e),
            }
        }
        _ => return None,
    })
}
