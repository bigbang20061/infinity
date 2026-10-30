//! Station ship boarding and assembly, mirroring the client ship service.
//! Uses game items and characters; corporation and structure access is unsupported.
use crate::services::dogma::ship_state;
use crate::services::inventory::notify_item_change;
use crate::services::{db_err, user_error, value_ids, Call, CallResult};
use crate::dao::items::loading::ItemQuery;
use crate::services::items::load_items;
use crate::dao::items::fitting as fitting_dao;
use crate::client::Client;
use crate::consts::{CATEGORY_SHIP, FLAG_CARGO, FLAG_HANGAR, IX_LOCATION_ID, IX_SINGLETON, IX_STACK_SIZE};
use crate::db::outbox::{dispatch_client, Outbox};
use crate::messages;
use marshal::Value;
use std::collections::BTreeMap;
use tracing::info;
/// SDE invGroups: Strategic Cruiser (inventorycommon/util.py:126-127).
const GROUP_STRATEGIC_CRUISER: i64 = 963;

/// Board(shipID, oldShipID) -> shipState: make an assembled ship in this station's hangar
/// the active one (clientDogmaLocation.MakeShipActive).
async fn board(c: &mut Client, ship_id: i64) -> CallResult {
    let char_id = c.session.int("charid").unwrap_or(0);
    let Some(station) = c.session.int("stationid") else { return Err(user_error(messages::MSG_CANT_BOARD_SHIP_WHILE_IN_SPACE, vec![])) };
    let ship = load_items(&c.db, &c.sde, ItemQuery::Id(ship_id)).await.map_err(db_err)?.into_iter().next().ok_or_else(|| user_error(messages::MSG_SHIP_NOT_FOUND, vec![]))?;
    if ship.category_id != CATEGORY_SHIP as i64 || ship.owner_id != char_id || ship.location_id != station {
        return Err(user_error(messages::MSG_CANT_BOARD_SHIP_NOT_IN_HANGAR, vec![]));
    }
    if !ship.singleton {
        return Err(user_error(messages::MSG_CANT_BOARD_UNASSEMBLED_SHIP, vec![]));
    }
    crate::dao::characters::selection::set_ship(&c.db, char_id, ship_id).await.map_err(db_err)?;
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
    let sde = std::sync::Arc::clone(&c.sde);
    let assembled = db.transaction(
        move |tx, outbox: &mut Outbox| {
            let (ids, name) = (ids.clone(), name.clone());
            let sde = std::sync::Arc::clone(&sde);
            Box::pin(async move {
                // Simulate requests in their original order, but write each source once
                // in primary-key order before inserting any generated hulls.
                let mut sources = BTreeMap::new();
                let mut actions = Vec::new();
                for id in ids {
                    if !sources.contains_key(&id) {
                        let item = load_items(tx, &sde, ItemQuery::Id(id)).await.map_err(db_err)?.into_iter().next()
                            .ok_or_else(|| crate::services::notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
                        let valid_source = if let Some(station) = station {
                            item.location_id == station && item.flag_id == FLAG_HANGAR
                        } else if Some(item.location_id) == active_ship && item.flag_id == FLAG_CARGO {
                            let carrier = load_items(tx, &sde, ItemQuery::Id(item.location_id)).await.map_err(db_err)?.into_iter().next();
                            carrier.is_some_and(|carrier| carrier.owner_id == char_id && carrier.category_id == CATEGORY_SHIP as i64)
                        } else {
                            false
                        };
                        if item.owner_id != char_id || !valid_source {
                            return Err(crate::services::notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
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
                if !fitting_dao::assemble_sources(tx, &sources, char_id, &name).await.map_err(db_err)? {
                    return Err(crate::services::notify_error(messages::INSUFFICIENT_ITEM_QUANTITY));
                }
                let mut assembled = Vec::new();
                let mut notified = std::collections::BTreeSet::new();
                for (id, split) in actions {
                    let (item, remaining) = &sources[&id];
                    if notified.insert(id) {
                        notify_item_change(tx, &sde, outbox, id, assembly_source_change(item.quantity, remaining.unwrap())).await?;
                    }
                    let ship_id = if split {
                        let new_id = fitting_dao::insert_assembled(tx, item, char_id, &name).await.map_err(db_err)?;
                        notify_item_change(tx, &sde, outbox, new_id, vec![(IX_LOCATION_ID, Value::Int(0))]).await?;
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

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    Some(match call.method {
        "Board" => board(c, call.args.first().and_then(|v| v.as_int()).unwrap_or(0)).await,
        "AssembleShip" => assemble(c, call).await,
        "CheckEnoughSubSystemToUndock" => Ok(Value::Bool(true)),
        _ => return None,
    })
}

#[cfg(test)] mod tests;
