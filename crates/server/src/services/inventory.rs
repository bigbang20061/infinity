//! invbroker: inventory managers (bound per station / solar system) hand out
//! bound inventories, which list and describe the items in a container
//! (eve/client/script/environment/invCache.py, invCacheContainer).

use super::dogma::{load_items, Item};
use super::{db_err, keyval, value_id_list, Call, CallError, CallResult};
use crate::client::{short, Client};
use crate::consts::{ATTR_CAPACITY, CATEGORY_SHIP, FLAG_CARGO, FLAG_CORP_SAG1, FLAG_FITTING_FIRST, FLAG_FITTING_LAST, FLAG_HANGAR, FLAG_PILOT, FLAG_RIG_FIRST, FLAG_RIG_LAST, FLAG_SUBSYSTEM_FIRST, FLAG_SUBSYSTEM_LAST, IX_FLAG, IX_LOCATION_ID, IX_SINGLETON, IX_STACK_SIZE, LOCATION_JUNKYARD, ROLE_DIRECTOR, TYPE_CHARACTER};
use marshal::Value;
use tracing::warn;

/// const.containerHangar: "my hangar in the current station".
const CONTAINER_HANGAR: i64 = 10004;
/// Existing server policy for the highest corporation-hangar division index.
const MAX_CORP_HANGAR_DIVISION_INDEX: i32 = 6;
/// Internal bound-object name for a single inventory.
pub const INVENTORY: &str = "invInventory";

/// Which container a bound inventory lists.
enum Container {
    /// The character's hangar in a station.
    Hangar { station_id: i64, owner_id: i64 },
    /// Any item acting as a container (ship, character, can...).
    Item(i64),
}

fn container_of(params: Option<&Value>) -> Option<Container> {
    let p = params?.items()?;
    match p {
        [Value::Int(station), Value::Int(CONTAINER_HANGAR), Value::Int(owner)] => {
            Some(Container::Hangar { station_id: *station, owner_id: *owner })
        }
        [Value::Int(item)] => Some(Container::Item(*item)),
        _ => None,
    }
}

fn kw_or_arg<'a>(call: &'a Call<'_>, name: &str, idx: usize) -> Option<&'a Value> {
    call.kwargs.get(name).or_else(|| call.args.get(idx)).filter(|v| !v.is_none())
}

async fn list(c: &Client, container: &Container, flags: Option<Vec<i64>>) -> Result<Vec<Item>, CallError> {
    let flags: Option<Vec<i32>> = flags.map(|f| f.into_iter().map(|x| x as i32).collect());
    match container {
        Container::Hangar { station_id, owner_id } => {
            let flags = flags.unwrap_or_else(|| vec![FLAG_HANGAR]);
            load_items(c, "i.location_id = $1 AND i.owner_id = $2 AND i.flag_id = ANY($3)", &[station_id, owner_id, &flags]).await
        }
        // Another corporation's hangars stay closed.
        Container::Item(item_id) if super::offices::office_owner(&c.db, *item_id).await?.is_some_and(|corp| c.session.int("corpid") != Some(corp)) => Ok(vec![]),
        Container::Item(item_id) => match flags {
            Some(flags) => load_items(c, "i.location_id = $1 AND i.flag_id = ANY($2)", &[item_id, &flags]).await,
            None => load_items(c, "i.location_id = $1", &[item_id]).await,
        },
    }
}

/// The container's own item row (`GetSelfInvItem`).
async fn self_item(c: &Client, container: &Container) -> Result<Value, CallError> {
    let id = match container {
        Container::Hangar { station_id, .. } => *station_id,
        Container::Item(id) => *id,
    };
    if let Some(item) = load_items(c, "i.item_id = $1", &[&id]).await?.into_iter().next() {
        return Ok(item.row());
    }
    if Some(id) == c.session.int("charid") {
        return Ok(Item {
            item_id: id,
            type_id: TYPE_CHARACTER,
            owner_id: id,
            location_id: c.session.int("shipid").unwrap_or(0),
            flag_id: FLAG_PILOT,
            quantity: 1,
            singleton: true,
            group_id: 1,
            category_id: 1,
            custom_info: None,
        }
        .row());
    }
    // A station (or other static location) as an item.
    let r = c
        .db
        .query_opt(
            "SELECT c.type_id, s.owner_id::bigint, c.solar_system_id::bigint, c.group_id::bigint, t.category_id::bigint
             FROM sde.map_celestials c JOIN sde.inv_types t USING (type_id)
             LEFT JOIN sde.sta_stations s ON s.station_id = c.item_id WHERE c.item_id = $1",
            &[&id],
        )
        .await
        .map_err(db_err)?;
    Ok(match r {
        Some(r) => Item {
            item_id: id,
            type_id: r.get::<_, Option<i32>>(0).unwrap_or(0),
            owner_id: r.get::<_, Option<i64>>(1).unwrap_or(1),
            location_id: r.get::<_, Option<i64>>(2).unwrap_or(0),
            flag_id: 0,
            quantity: 1,
            singleton: true,
            group_id: r.get::<_, Option<i64>>(3).unwrap_or(0),
            category_id: r.get::<_, Option<i64>>(4).unwrap_or(0),
            custom_info: None,
        }
        .row(),
        None => Value::None,
    })
}

async fn capacity(c: &Client, container: &Container) -> Result<Value, CallError> {
    let (cap, used) = match container {
        Container::Hangar { .. } => (1e12, 0.0),
        Container::Item(id) => {
            let r = c
                .db
                .query_opt(
                    "SELECT COALESCE((SELECT value FROM sde.type_attributes a WHERE a.type_id = i.type_id AND a.attribute_id = $2),
                                     t.capacity, 0),
                            COALESCE((SELECT sum(t2.volume * GREATEST(c.quantity, 1)) FROM game.items c
                                      JOIN sde.inv_types t2 USING (type_id) WHERE c.location_id = i.item_id), 0)
                     FROM game.items i JOIN sde.inv_types t USING (type_id) WHERE i.item_id = $1",
                    &[id, &ATTR_CAPACITY],
                )
                .await
                .map_err(db_err)?;
            r.map(|r| (r.get::<_, f64>(0), r.get::<_, f64>(1))).unwrap_or((0.0, 0.0))
        }
    };
    Ok(keyval([("capacity", Value::Float(cap)), ("used", Value::Float(used))]))
}

// ---------------------------------------------------------------- moving items

const FLAG_AUTOFIT: i32 = 0;
/// appConst.corpRoleHangarCanTake1; divisions 2-7 are the next bits.
const ROLE_HANGAR_CAN_TAKE_1: i64 = 8192;

/// Fitting slot flags: low, medium, high, rig and subsystem slots.
pub fn is_fitting_flag(flag: i32) -> bool {
    (FLAG_FITTING_FIRST..=FLAG_FITTING_LAST).contains(&flag) || (FLAG_RIG_FIRST..=FLAG_RIG_LAST).contains(&flag) || (FLAG_SUBSYSTEM_FIRST..=FLAG_SUBSYSTEM_LAST).contains(&flag)
}

/// `OnItemChange(item, change, location)` for `item_id` as it is now in the database.
pub async fn notify_item_change(c: &mut Client, item_id: i64, change: Vec<(i64, Value)>) -> Result<(), CallError> {
    if let Some(item) = load_items(c, "i.item_id = $1", &[&item_id]).await?.into_iter().next() {
        let change = Value::Dict(change.into_iter().map(|(k, v)| (Value::Int(k), v)).collect());
        c.notify("OnItemChange", vec![item.row(), change, Value::None]);
    }
    Ok(())
}

/// Tell the client an item is gone (moved to the junkyard), then delete it.
async fn destroy_item(c: &mut Client, item: Item) -> Result<(), CallError> {
    c.db.execute("DELETE FROM game.items WHERE item_id = $1", &[&item.item_id]).await.map_err(db_err)?;
    let old_location = item.location_id;
    let gone = Item { location_id: LOCATION_JUNKYARD, ..item };
    let change = Value::Dict(vec![(Value::Int(IX_LOCATION_ID), Value::Int(old_location))]);
    c.notify("OnItemChange", vec![gone.row(), change, Value::None]);
    Ok(())
}

/// The first free slot on `ship` for a module, from the module's slot effect and the
/// ship's slot counts (lowSlots/medSlots/hiSlots/rigSlots/maxSubSystems).
async fn free_slot(c: &Client, ship: i64, module_type: i32) -> Result<Option<i32>, CallError> {
    // (slot effect, ship slot-count attribute, first flag, max slots)
    const SLOTS: [(i32, i32, i32, i32); 5] = [(11, 12, 11, 8), (13, 13, 19, 8), (12, 14, 27, 8), (2663, 1137, 92, 8), (3772, 1367, 125, 8)];
    let effects: Vec<i32> = c
        .db
        .query("SELECT effect_id FROM sde.type_effects WHERE type_id = $1", &[&module_type])
        .await
        .map_err(db_err)?
        .iter()
        .map(|r| r.get(0))
        .collect();
    let Some(&(_, count_attr, first, max)) = SLOTS.iter().find(|s| effects.contains(&s.0)) else { return Ok(None) };
    let count: f64 = c
        .db
        .query_opt(
            "SELECT a.value FROM game.items i JOIN sde.type_attributes a ON a.type_id = i.type_id AND a.attribute_id = $2 WHERE i.item_id = $1",
            &[&ship, &count_attr],
        )
        .await
        .map_err(db_err)?
        .map(|r| r.get(0))
        .unwrap_or(0.0);
    let used: Vec<i32> = c
        .db
        .query("SELECT flag_id FROM game.items WHERE location_id = $1", &[&ship])
        .await
        .map_err(db_err)?
        .iter()
        .map(|r| r.get(0))
        .collect();
    Ok((first..first + (count as i32).min(max)).find(|f| !used.contains(f)))
}

/// Move `item_id` (all of it, or `qty` split off a stack) into `container` under `flag`.
/// Returns the ID of the item now at the destination.
async fn move_item(c: &mut Client, container: &Container, item_id: i64, flag: Option<i32>, qty: Option<i64>) -> Result<Option<i64>, CallError> {
    let char_id = c.session.int("charid").unwrap_or(0);
    let Some(item) = load_items(c, "i.item_id = $1", &[&item_id]).await?.into_iter().next() else { return Ok(None) };
    if Some(item_id) == c.session.int("shipid") && matches!(container, Container::Item(_)) {
        return Err(CallError::User { msg: "CanNotPlaceActiveShipInAnotherHangar".into(), dict: vec![] });
    }
    // Out of a corporation hangar: the division's take role (or Director) is needed.
    if let Some(corp) = super::offices::office_owner(&c.db, item.location_id).await? {
        let (my_corp, roles) = super::corps::membership(&c.db, char_id).await?;
        let take = ROLE_HANGAR_CAN_TAKE_1 << (item.flag_id - FLAG_CORP_SAG1).clamp(0, MAX_CORP_HANGAR_DIVISION_INDEX);
        if my_corp != corp || roles & (take | ROLE_DIRECTOR) == 0 {
            return Err(CallError::User { msg: "CustomNotify".into(), dict: vec![("notify".into(), Value::unicode("你没有从该军团机库部门取出物品的职权。"))] });
        }
    }
    let (location, owner, mut flag) = match container {
        Container::Hangar { station_id, owner_id } => (*station_id, *owner_id, flag.filter(|f| *f != FLAG_AUTOFIT).unwrap_or(FLAG_HANGAR)),
        Container::Item(id) => match super::offices::office_owner(&c.db, *id).await? {
            // Into a corporation hangar: the corporation owns what's in it.
            Some(corp) => {
                if c.session.int("corpid") != Some(corp) {
                    return Err(CallError::User { msg: "CustomNotify".into(), dict: vec![("notify".into(), Value::unicode("这不是你军团的机库。"))] });
                }
                let sag = FLAG_CORP_SAG1;
                (*id, corp, flag.filter(|f| (sag..sag + 7).contains(f)).unwrap_or(sag))
            }
            None => {
                let is_ship = load_items(c, "i.item_id = $1", &[id]).await?.first().is_some_and(|i| i.category_id == CATEGORY_SHIP as i64);
                let default = if is_ship { FLAG_CARGO } else { FLAG_HANGAR };
                (*id, char_id, flag.unwrap_or(default))
            }
        },
    };
    if flag == FLAG_AUTOFIT {
        flag = match free_slot(c, location, item.type_id).await? {
            Some(slot) => slot,
            None => FLAG_CARGO,
        };
    }
    let fitting = is_fitting_flag(flag);
    // Fitting takes one module off a stack; otherwise the requested quantity, or all of it.
    let take = if fitting { Some(1) } else { qty }.filter(|q| *q > 0 && !item.singleton && (*q as i32) < item.quantity);
    let moved_id = if let Some(q) = take {
        c.db
            .execute("UPDATE game.items SET quantity = quantity - $2 WHERE item_id = $1", &[&item_id, &(q as i32)])
            .await
            .map_err(db_err)?;
        notify_item_change(c, item_id, vec![(IX_STACK_SIZE, Value::Int(item.quantity as i64))]).await?;
        let new_id: i64 = c
            .db
            .query_one(
                "INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) VALUES ($1, $2, $3, $4, $5, $6) RETURNING item_id",
                &[&item.type_id, &owner, &location, &flag, &(q as i32), &fitting],
            )
            .await
            .map_err(db_err)?
            .get(0);
        notify_item_change(c, new_id, vec![(IX_LOCATION_ID, Value::Int(item.location_id)), (IX_FLAG, Value::Int(item.flag_id as i64))]).await?;
        new_id
    } else {
        let singleton = item.singleton || fitting;
        c.db
            .execute(
                "UPDATE game.items SET location_id = $2, flag_id = $3, owner_id = $4, singleton = $5,
                        online = online AND $6, x = NULL, y = NULL, z = NULL WHERE item_id = $1",
                &[&item_id, &location, &flag, &owner, &singleton, &fitting],
            )
            .await
            .map_err(db_err)?;
        let mut change = vec![(IX_LOCATION_ID, Value::Int(item.location_id)), (IX_FLAG, Value::Int(item.flag_id as i64))];
        if singleton != item.singleton {
            change.push((IX_SINGLETON, Value::Bool(item.singleton)));
        }
        notify_item_change(c, item_id, change).await?;
        item_id
    };
    Ok(Some(moved_id))
}

/// `MultiMerge([(sourceID, destID, qty)], sourceContainerID)`: pour stacks into stacks of the same type.
async fn merge(c: &mut Client, source_id: i64, dest_id: i64, qty: i64) -> Result<(), CallError> {
    let items = load_items(c, "i.item_id = ANY($1)", &[&vec![source_id, dest_id]]).await?;
    let (Some(src), Some(dst)) = (items.iter().find(|i| i.item_id == source_id), items.iter().find(|i| i.item_id == dest_id)) else { return Ok(()) };
    if src.type_id != dst.type_id || src.singleton || dst.singleton || source_id == dest_id {
        return Ok(());
    }
    let qty = qty.clamp(1, src.quantity as i64) as i32;
    let (dst_old, src_old) = (dst.quantity, src.quantity);
    c.db.execute("UPDATE game.items SET quantity = quantity + $2 WHERE item_id = $1", &[&dest_id, &qty]).await.map_err(db_err)?;
    notify_item_change(c, dest_id, vec![(IX_STACK_SIZE, Value::Int(dst_old as i64))]).await?;
    if qty >= src_old {
        let src = load_items(c, "i.item_id = $1", &[&source_id]).await?.into_iter().next();
        if let Some(src) = src {
            destroy_item(c, src).await?;
        }
    } else {
        c.db.execute("UPDATE game.items SET quantity = quantity - $2 WHERE item_id = $1", &[&source_id, &qty]).await.map_err(db_err)?;
        notify_item_change(c, source_id, vec![(IX_STACK_SIZE, Value::Int(src_old as i64))]).await?;
    }
    Ok(())
}

/// `StackAll([flag])`: merge every unassembled stack of the same type in the container.
async fn stack_all(c: &mut Client, container: &Container, flag: Option<i64>) -> Result<(), CallError> {
    let items = list(c, container, flag.map(|f| vec![f])).await?;
    let mut first: std::collections::HashMap<(i32, i32), i64> = Default::default();
    for item in items.iter().filter(|i| !i.singleton) {
        match first.get(&(item.type_id, item.flag_id)) {
            Some(&dest) => merge(c, item.item_id, dest, item.quantity as i64).await?,
            None => {
                first.insert((item.type_id, item.flag_id), item.item_id);
            }
        }
    }
    Ok(())
}

/// Calls on the inventory manager (`invbroker` bound to a location).
pub async fn broker(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let char_id = c.session.int("charid").unwrap_or(0);
    Some(Ok(match call.method {
        "GetInventoryFromId" => {
            let id = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
            if super::exploration::container_locked(id) {
                let msg = Value::unicode("这个容器被锁住了。用数据分析仪或遗迹分析仪破解它。");
                return Some(Err(CallError::User { msg: "CustomNotify".into(), dict: vec![("notify".into(), msg)] }));
            }
            c.bind(INVENTORY, Value::tuple([Value::Int(id)]))
        }
        "GetInventory" => {
            let container = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
            if container == CONTAINER_HANGAR {
                let station = c.session.int("stationid").unwrap_or(0);
                c.bind(INVENTORY, Value::tuple([Value::Int(station), Value::Int(CONTAINER_HANGAR), Value::Int(char_id)]))
            } else {
                warn!(user = c.login.user_id, "invbroker::GetInventory for unsupported container {container}");
                c.bind(INVENTORY, Value::tuple([Value::Int(container)]))
            }
        }
        "GetItemDescriptor" => super::item_descriptor().to_value(),
        "IsMyItemAtDockableLocation" => Value::Bool(true),
        // SplitStack(locationID, itemID, qty, ownerID): a new stack beside the old one.
        "SplitStack" => {
            let item_id = call.args.get(1).and_then(|v| v.as_int()).unwrap_or(0);
            let qty = call.args.get(2).and_then(|v| v.as_int()).unwrap_or(0) as i32;
            let Some(item) = (match load_items(c, "i.item_id = $1", &[&item_id]).await {
                Ok(items) => items.into_iter().next(),
                Err(e) => return Some(Err(e)),
            }) else {
                return Some(Ok(Value::None));
            };
            if item.singleton || qty <= 0 || qty >= item.quantity {
                return Some(Ok(Value::None));
            }
            let r = async {
                c.db
                    .execute("UPDATE game.items SET quantity = quantity - $2 WHERE item_id = $1", &[&item_id, &qty])
                    .await
                    .map_err(db_err)?;
                notify_item_change(c, item_id, vec![(IX_STACK_SIZE, Value::Int(item.quantity as i64))]).await?;
                let new_id: i64 = c
                    .db
                    .query_one(
                        "INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity) VALUES ($1, $2, $3, $4, $5) RETURNING item_id",
                        &[&item.type_id, &item.owner_id, &item.location_id, &item.flag_id, &qty],
                    )
                    .await
                    .map_err(db_err)?
                    .get(0);
                notify_item_change(c, new_id, vec![(IX_LOCATION_ID, Value::Int(0))]).await?;
                Ok::<_, CallError>(Value::Int(new_id))
            }
            .await;
            return Some(r);
        }
        // TrashItems(itemIDs, locationID) -> [error messages]
        "TrashItems" => {
            let ids = value_id_list(call.args.first());
            let r = async {
                let items = load_items(c, "i.item_id = ANY($1) AND i.owner_id = $2", &[&ids, &char_id]).await?;
                for item in items {
                    if Some(item.item_id) != c.session.int("shipid") {
                        destroy_item(c, item).await?;
                    }
                }
                Ok::<_, CallError>(Value::list([]))
            }
            .await;
            return Some(r);
        }
        // SetLabel(itemID, name): ship and container names.
        "SetLabel" => {
            let item_id = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
            let name = call.args.get(1).and_then(|v| v.as_text()).map(|t| t.into_owned()).unwrap_or_default();
            if let Err(e) = c.db.execute("UPDATE game.items SET name = $2 WHERE item_id = $1 AND owner_id = $3", &[&item_id, &name, &char_id]).await {
                return Some(Err(db_err(e)));
            }
            Value::None
        }
        _ => return None,
    }))
}

/// Calls on a bound inventory.
pub async fn inventory(c: &mut Client, call: &Call<'_>) -> CallResult {
    let Some(container) = container_of(call.bind_params) else {
        return Err(CallError::Internal(anyhow::anyhow!("bad inventory bind params {:?}", call.bind_params)));
    };
    match call.method {
        "List" => {
            let flag = kw_or_arg(call, "flag", 0).and_then(|v| v.as_int());
            let items = list(c, &container, flag.map(|f| vec![f])).await?;
            Ok(Value::List(items.iter().map(Item::row).collect()))
        }
        "ListByFlags" => {
            let flags = kw_or_arg(call, "flags", 0).and_then(|v| v.items()).map(|f| f.iter().filter_map(|x| x.as_int()).collect());
            let items = list(c, &container, flags).await?;
            Ok(Value::List(items.iter().map(Item::row).collect()))
        }
        "GetSelfInvItem" => self_item(c, &container).await,
        "GetItem" => self_item(c, &container).await,
        "GetCapacity" => capacity(c, &container).await,
        "GetAvailableTurretSlots" => Ok(Value::list([])),
        // Add(itemID, sourceID, flag=, qty=) -> itemID now in this container
        "Add" => {
            let item_id = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
            let flag = call.kwargs.get("flag").and_then(|v| v.as_int()).map(|f| f as i32);
            let qty = call.kwargs.get("qty").or_else(|| call.kwargs.get("quantity")).and_then(|v| v.as_int());
            move_item(c, &container, item_id, flag, qty).await.map(|id| id.map(Value::Int).unwrap_or(Value::None))
        }
        // MultiAdd(itemIDs, sourceID, flag=)
        "MultiAdd" => {
            let flag = call.kwargs.get("flag").and_then(|v| v.as_int()).map(|f| f as i32);
            for item_id in value_id_list(call.args.first()) {
                move_item(c, &container, item_id, flag, None).await?;
            }
            Ok(Value::None)
        }
        // MultiMerge([(sourceID, destID, qty)], sourceContainerID)
        "MultiMerge" => {
            let ops: Vec<(i64, i64, i64)> = call
                .args
                .first()
                .and_then(|v| v.items())
                .unwrap_or_default()
                .iter()
                .filter_map(|op| match op.items()? {
                    [s, d, q] => Some((s.as_int()?, d.as_int()?, q.as_int()?)),
                    _ => None,
                })
                .collect();
            for (source, dest, qty) in ops {
                merge(c, source, dest, qty).await?;
            }
            Ok(Value::None)
        }
        "StackAll" => {
            let flag = kw_or_arg(call, "flag", 0).and_then(|v| v.as_int());
            stack_all(c, &container, flag).await.map(|_| Value::None)
        }
        // {itemID: ...} maps; nothing soul-bound or service-life damaged.
        "ListSoulBound" | "ListServiceLifeDamage" => Ok(Value::Dict(vec![])),
        _ => {
            warn!(
                user = c.login.user_id,
                "UNHANDLED inventory::{} args={} kwargs={}",
                call.method,
                short(&marshal::Value::Tuple(call.args.to_vec())),
                short(call.kwargs)
            );
            Ok(Value::None)
        }
    }
}
