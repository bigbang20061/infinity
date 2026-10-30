//! Item repackaging, mirroring the client repackagingSvc service.
//! Uses game items and SDE stations; structure access is unsupported.
use crate::services::inventory::notify_item_change;
use crate::services::{db_err, user_error, Call, CallError, CallResult};
use crate::dao::items::loading::{load_items, ItemQuery};
use crate::dao::items::fitting as fitting_dao;
use crate::client::Client;
use crate::consts::{CATEGORY_SHIP, FLAG_HANGAR, IX_SINGLETON};
use crate::db::outbox::{dispatch_client, Outbox};
use crate::messages;
use marshal::Value;

/// RepackageItems({stationID: [(itemID, locationID)]}, destroyable=): unassemble ships and modules.
async fn repackage(c: &mut Client, call: &Call<'_>) -> CallResult {
    let char_id = c.session.int("charid").unwrap_or(0);
    let active_ship = c.session.int("shipid");
    let mut ids = vec![];
    if let Some(Value::Dict(by_station)) = call.args.first() {
        for (named_station, items) in by_station {
            let named_station = named_station.as_int().ok_or_else(|| crate::services::notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
            for pair in items.items().unwrap_or_default() {
                let pair = pair.items().unwrap_or_default();
                let id = pair.first().and_then(|v| v.as_int()).ok_or_else(|| crate::services::notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
                let location = pair.get(1).and_then(|v| v.as_int()).ok_or_else(|| crate::services::notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
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
            .ok_or_else(|| crate::services::notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
        if item.owner_id != char_id || item.location_id != named_station || item.location_id != location || item.flag_id != FLAG_HANGAR
            || c.db.query_opt("SELECT 1 FROM sde.sta_stations WHERE station_id = $1::bigint", &[&named_station]).await.map_err(db_err)?.is_none() {
            return Err(crate::services::notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
        }
        if planned.contains_key(&id) {
            continue;
        }
        if !item.singleton {
            continue;
        }
        if item.category_id == CATEGORY_SHIP as i64 {
            let fitted = fitting_dao::fitted_count(&c.db, id).await.map_err(db_err)?;
            if fitted > 0 {
                return Err(CallError::User { msg: messages::MSG_CONFIRM_REPACKAGE_SOMETHING_WITH_UPGRADES.into(), dict: vec![] });
            }
        }
        planned.insert(id, item.location_id);
    }
    for (id, station) in planned {
        let changed = fitting_dao::repackage(&c.db, id, char_id, station).await.map_err(db_err)?;
        if changed == 0 {
            return Err(crate::services::notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
        }
        let mut outbox = Outbox::default();
        notify_item_change(&c.db, &mut outbox, id, vec![(IX_SINGLETON, Value::Bool(true))]).await?;
        dispatch_client((), outbox, c)?;
    }
    Ok(Value::None)
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.method { "RepackageItems" => Some(repackage(c, call).await), _ => None }
}
