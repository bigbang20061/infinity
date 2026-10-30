//! invbroker: inventory managers (bound per station / solar system) hand out
//! bound inventories, which list and describe the items in a container
//! (eve/client/script/environment/invCache.py, invCacheContainer).
//! Item quantities are stored in `game.items`; merge plans are guarded against
//! stale snapshots. Other inventory operations retain their existing semantics.

use crate::dao::items::loading::{load_items, Item, ItemQuery};
use crate::dao::items::inventory as inventory_dao;
use crate::dao::items::quantities::{self, QuantityChange};
use super::{db_err, keyval, notify_error, value_id_list, Call, CallError, CallResult};
use crate::client::{short, Client};
use crate::consts::{ATTR_CAPACITY, CATEGORY_SHIP, FLAG_CARGO, FLAG_CORP_SAG1, FLAG_FITTING_FIRST, FLAG_FITTING_LAST, FLAG_HANGAR, FLAG_PILOT, FLAG_RIG_FIRST, FLAG_RIG_LAST, FLAG_SUBSYSTEM_FIRST, FLAG_SUBSYSTEM_LAST, IX_FLAG, IX_LOCATION_ID, IX_SINGLETON, IX_STACK_SIZE, LOCATION_JUNKYARD, ROLE_DIRECTOR, TYPE_CHARACTER};
use crate::db::executor::Executor;
use crate::db::outbox::{dispatch_client, Outbox};
use crate::messages;
use marshal::Value;
use std::collections::BTreeMap;
use tracing::warn;

/// const.containerHangar: "my hangar in the current station".
const CONTAINER_HANGAR: i64 = 10004;
/// Existing server policy for the highest corporation-hangar division index.
const MAX_CORP_HANGAR_DIVISION_INDEX: i32 = 6;
/// Internal bound-object name for a single inventory.
pub const INVENTORY: &str = "invInventory";

/// Which container a bound inventory lists.
#[derive(Clone)]
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
            load_items(&c.db, ItemQuery::LocationOwnerFlags(*station_id, *owner_id, &flags)).await.map_err(db_err)
        }
        // Another corporation's hangars stay closed.
        Container::Item(item_id) if super::offices::office_owner(&c.db, *item_id).await?.is_some_and(|corp| c.session.int("corpid") != Some(corp)) => Ok(vec![]),
        Container::Item(item_id) => match flags {
            Some(flags) => load_items(&c.db, ItemQuery::LocationFlags(*item_id, &flags)).await.map_err(db_err),
            None => load_items(&c.db, ItemQuery::Location(*item_id)).await.map_err(db_err),
        },
    }
}

/// The container's own item row (`GetSelfInvItem`).
async fn self_item(c: &Client, container: &Container) -> Result<Value, CallError> {
    let id = match container {
        Container::Hangar { station_id, .. } => *station_id,
        Container::Item(id) => *id,
    };
    if let Some(item) = load_items(&c.db, ItemQuery::Id(id)).await.map_err(db_err)?.into_iter().next() {
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
            type_id: r.get::<_, Option<i32>>("type_id").unwrap_or(0),
            owner_id: r.get::<_, Option<i64>>("owner_id").unwrap_or(1),
            location_id: r.get::<_, Option<i64>>("solar_system_id").unwrap_or(0),
            flag_id: 0,
            quantity: 1,
            singleton: true,
            group_id: r.get::<_, Option<i64>>("group_id").unwrap_or(0),
            category_id: r.get::<_, Option<i64>>("category_id").unwrap_or(0),
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
            inventory_dao::capacity(&c.db, *id, ATTR_CAPACITY).await.map_err(db_err)?
                .map(|r| (r.capacity, r.used)).unwrap_or((0.0, 0.0))
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
pub async fn notify_item_change<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, item_id: i64, change: Vec<(i64, Value)>) -> Result<(), CallError> {
    if let Some(item) = load_items(db, ItemQuery::Id(item_id)).await.map_err(db_err)?.into_iter().next() {
        let change = Value::Dict(change.into_iter().map(|(k, v)| (Value::Int(k), v)).collect());
        outbox.notify_client("OnItemChange", vec![item.row(), change, Value::None]);
    }
    Ok(())
}

/// Queue the standard junkyard notification for a removed item.
pub(super) fn notify_item_removed<X: Send>(outbox: &mut Outbox<X>, item: Item) {
    let old_location = item.location_id;
    let gone = Item { location_id: LOCATION_JUNKYARD, ..item };
    let change = Value::Dict(vec![(Value::Int(IX_LOCATION_ID), Value::Int(old_location))]);
    outbox.notify_client("OnItemChange", vec![gone.row(), change, Value::None]);
}

pub(super) async fn split_stack(db: &(impl Executor + ?Sized), item: &Item, quantity: i32, owner: i64, location: i64, flag: i32, singleton: bool) -> Result<i64, CallError> {
    inventory_dao::split(db, item, quantity, owner, location, flag, singleton).await.map_err(db_err)?
        .ok_or_else(|| notify_error(messages::STACK_CANNOT_BE_SPLIT))
}

/// Tell the client an item is gone (moved to the junkyard), then delete it.
async fn destroy_item(c: &mut Client, item: Item) -> Result<(), CallError> {
    inventory_dao::delete(&c.db, item.item_id).await.map_err(db_err)?;
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
        .map(|r| r.get("effect_id"))
        .collect();
    let Some(&(_, count_attr, first, max)) = SLOTS.iter().find(|s| effects.contains(&s.0)) else { return Ok(None) };
    let count = inventory_dao::slot_count(&c.db, ship, count_attr).await.map_err(db_err)?
        .map(|r| r.value).unwrap_or(0.0);
    let used: Vec<i32> = inventory_dao::slot_flags(&c.db, ship).await.map_err(db_err)?
        .into_iter().map(|r| r.flag_id).collect();
    Ok((first..first + (count as i32).min(max)).find(|f| !used.contains(f)))
}

/// Move `item_id` (all of it, or `qty` split off a stack) into `container` under `flag`.
/// Returns the ID of the item now at the destination.
async fn move_item(c: &mut Client, container: &Container, item_id: i64, flag: Option<i32>, qty: Option<i64>) -> Result<Option<i64>, CallError> {
    let char_id = c.session.int("charid").unwrap_or(0);
    let Some(item) = load_items(&c.db, ItemQuery::Id(item_id)).await.map_err(db_err)?.into_iter().next() else { return Ok(None) };
    if Some(item_id) == c.session.int("shipid") && matches!(container, Container::Item(_)) {
        return Err(CallError::User { msg: messages::MSG_CAN_NOT_PLACE_ACTIVE_SHIP_IN_ANOTHER_HANGAR.into(), dict: vec![] });
    }
    // Out of a corporation hangar: the division's take role (or Director) is needed.
    if let Some(corp) = super::offices::office_owner(&c.db, item.location_id).await? {
        let (my_corp, roles) = super::corps::membership(&c.db, char_id).await?;
        let take = ROLE_HANGAR_CAN_TAKE_1 << (item.flag_id - FLAG_CORP_SAG1).clamp(0, MAX_CORP_HANGAR_DIVISION_INDEX);
        if my_corp != corp || roles & (take | ROLE_DIRECTOR) == 0 {
            return Err(CallError::User { msg: messages::MSG_CUSTOM_NOTIFY.into(), dict: vec![("notify".into(), Value::unicode(messages::NO_CORPORATION_HANGAR_TAKE_ROLE))] });
        }
    }
    let (location, owner, mut flag) = match container {
        Container::Hangar { station_id, owner_id } => (*station_id, *owner_id, flag.filter(|f| *f != FLAG_AUTOFIT).unwrap_or(FLAG_HANGAR)),
        Container::Item(id) => match super::offices::office_owner(&c.db, *id).await? {
            // Into a corporation hangar: the corporation owns what's in it.
            Some(corp) => {
                if c.session.int("corpid") != Some(corp) {
                    return Err(CallError::User { msg: messages::MSG_CUSTOM_NOTIFY.into(), dict: vec![("notify".into(), Value::unicode(messages::NOT_YOUR_CORPORATION_HANGAR))] });
                }
                let sag = FLAG_CORP_SAG1;
                (*id, corp, flag.filter(|f| (sag..sag + 7).contains(f)).unwrap_or(sag))
            }
            None => {
                let is_ship = load_items(&c.db, ItemQuery::Id(*id)).await.map_err(db_err)?.first().is_some_and(|i| i.category_id == CATEGORY_SHIP as i64);
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
    let db = c.db.clone();
    db.transaction(
        move |tx, outbox: &mut Outbox| {
            let item = item.clone();
            Box::pin(async move {
                let moved_id = if let Some(q) = take {
                    let id = split_stack(tx, &item, q as i32, owner, location, flag, fitting).await?;
                    notify_item_change(tx, outbox, item_id, vec![(IX_STACK_SIZE, Value::Int(item.quantity as i64))]).await?;
                    notify_item_change(tx, outbox, id, vec![(IX_LOCATION_ID, Value::Int(item.location_id)), (IX_FLAG, Value::Int(item.flag_id as i64))]).await?;
                    id
                } else {
                    let singleton = item.singleton || fitting;
                    inventory_dao::move_item(tx, item_id, location, flag, owner, singleton, fitting).await.map_err(db_err)?;
                    let mut change = vec![(IX_LOCATION_ID, Value::Int(item.location_id)), (IX_FLAG, Value::Int(item.flag_id as i64))];
                    if singleton != item.singleton {
                        change.push((IX_SINGLETON, Value::Bool(item.singleton)));
                    }
                    notify_item_change(tx, outbox, item_id, change).await?;
                    item_id
                };
                Ok(Some(moved_id))
            })
        },
        |value, outbox| async move { dispatch_client(value, outbox, c) },
    ).await
}

/// Simulate transfers in request order, retaining the original snapshots for guarded writes.
fn plan_merges(items: &[Item], ops: &[(i64, i64, i64)]) -> Result<BTreeMap<i64, QuantityChange>, CallError> {
    let mut states: BTreeMap<i64, (&Item, Option<i32>)> = items.iter().map(|item| (item.item_id, (item, Some(item.quantity)))).collect();
    for &(source, dest, requested) in ops {
        if source == dest { continue; }
        let (Some(&(src, Some(src_qty))), Some(&(dst, Some(dst_qty)))) = (states.get(&source), states.get(&dest)) else { continue };
        if src.type_id != dst.type_id || src.singleton || dst.singleton || src_qty <= 0 { continue; }
        let take = requested.clamp(1, i64::from(src_qty)) as i32;
        let total = dst_qty.checked_add(take).ok_or_else(|| notify_error(messages::INSUFFICIENT_ITEM_QUANTITY))?;
        states.get_mut(&source).unwrap().1 = if take == src_qty { None } else { Some(src_qty - take) };
        states.get_mut(&dest).unwrap().1 = Some(total);
    }
    Ok(states.into_iter().filter_map(|(id, (item, quantity))| {
        (quantity != Some(item.quantity)).then(|| (id, QuantityChange::new(item, quantity)))
    }).collect())
}

async fn apply_merges<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, items: Vec<Item>, ops: &[(i64, i64, i64)]) -> Result<(), CallError> {
    let changes = plan_merges(&items, ops)?;
    if !quantities::apply(db, &changes).await.map_err(db_err)? { return Err(notify_error(messages::INSUFFICIENT_ITEM_QUANTITY)); }
    for item in items {
        if let Some(change) = changes.get(&item.item_id) {
            if change.quantity.is_none() {
                notify_item_removed(outbox, item);
            } else {
                notify_item_change(db, outbox, item.item_id, vec![(IX_STACK_SIZE, Value::Int(item.quantity as i64))]).await?;
            }
        }
    }
    Ok(())
}

async fn merge_operations(c: &mut Client, ops: Vec<(i64, i64, i64)>) -> Result<(), CallError> {
    let db = c.db.clone();
    db.transaction(
        move |tx, outbox: &mut Outbox| {
            let ops = ops.clone();
            Box::pin(async move {
                let ids: Vec<i64> = ops.iter().flat_map(|&(source, dest, _)| [source, dest]).collect();
                let items = load_items(tx, ItemQuery::Ids(&ids)).await.map_err(db_err)?;
                apply_merges(tx, outbox, items, &ops).await
            })
        },
        |value, outbox| async move { dispatch_client(value, outbox, c) },
    ).await
}

/// `StackAll([flag])`: merge every unassembled stack of the same type in the container.
async fn stack_all(c: &mut Client, container: &Container, flag: Option<i64>) -> Result<(), CallError> {
    if let Container::Item(id) = container {
        if super::offices::office_owner(&c.db, *id).await?.is_some_and(|corp| c.session.int("corpid") != Some(corp)) {
            return Ok(());
        }
    }
    let container = container.clone();
    let db = c.db.clone();
    db.transaction(
        move |tx, outbox: &mut Outbox| {
            let container = container.clone();
            Box::pin(async move {
                let flags: Option<Vec<i32>> = flag.map(|f| vec![f as i32]);
                let items = match container {
                    Container::Hangar { station_id, owner_id } => {
                        let flags = flags.unwrap_or_else(|| vec![FLAG_HANGAR]);
                        load_items(tx, ItemQuery::LocationOwnerFlags(station_id, owner_id, &flags)).await.map_err(db_err)?
                    }
                    Container::Item(id) => match flags {
                        Some(flags) => load_items(tx, ItemQuery::LocationFlags(id, &flags)).await.map_err(db_err)?,
                        None => load_items(tx, ItemQuery::Location(id)).await.map_err(db_err)?,
                    },
                };
                let mut first: std::collections::HashMap<(i32, i32), i64> = Default::default();
                let mut ops = Vec::new();
                for item in items.iter().filter(|i| !i.singleton) {
                    match first.get(&(item.type_id, item.flag_id)) {
                        Some(&dest) => ops.push((item.item_id, dest, item.quantity as i64)),
                        None => { first.insert((item.type_id, item.flag_id), item.item_id); }
                    }
                }
                apply_merges(tx, outbox, items, &ops).await
            })
        },
        |value, outbox| async move { dispatch_client(value, outbox, c) },
    ).await
}

/// Calls on the inventory manager (`invbroker` bound to a location).
pub async fn broker(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let char_id = c.session.int("charid").unwrap_or(0);
    Some(Ok(match call.method {
        "GetInventoryFromId" => {
            let id = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
            if super::exploration::container_locked(id) {
                let msg = Value::unicode(messages::CONTAINER_LOCKED_NEEDS_ANALYZER);
                return Some(Err(CallError::User { msg: messages::MSG_CUSTOM_NOTIFY.into(), dict: vec![("notify".into(), msg)] }));
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
            let Some(item) = (match load_items(&c.db, ItemQuery::Id(item_id)).await.map_err(db_err) {
                Ok(items) => items.into_iter().next(),
                Err(e) => return Some(Err(e)),
            }) else {
                return Some(Ok(Value::None));
            };
            if item.singleton || qty <= 0 || qty >= item.quantity {
                return Some(Ok(Value::None));
            }
            let db = c.db.clone();
            let r = db.transaction(
                move |tx, outbox: &mut Outbox| {
                    let item = item.clone();
                    Box::pin(async move {
                        let (owner, location, flag) = (item.owner_id, item.location_id, item.flag_id);
                        let id = split_stack(tx, &item, qty, owner, location, flag, false).await?;
                        notify_item_change(tx, outbox, item_id, vec![(IX_STACK_SIZE, Value::Int(item.quantity as i64))]).await?;
                        notify_item_change(tx, outbox, id, vec![(IX_LOCATION_ID, Value::Int(0))]).await?;
                        Ok(Value::Int(id))
                    })
                },
                |value, outbox| async move { dispatch_client(value, outbox, c) },
            ).await;
            return Some(r);
        }
        // TrashItems(itemIDs, locationID) -> [error messages]
        "TrashItems" => {
            let ids = value_id_list(call.args.first());
            let r = async {
                let items = load_items(&c.db, ItemQuery::IdsOwner(&ids, char_id)).await.map_err(db_err)?;
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
            if let Err(e) = inventory_dao::rename(&c.db, item_id, &name, char_id).await {
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
            merge_operations(c, ops).await?;
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

#[cfg(test)] mod tests;
