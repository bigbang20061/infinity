//! Shared item transfer policy for market and combat services, not a direct client module mirror.
//! Uses game.items, game.characters and sde.inv_types through the item DAO.
//! Callers own transactions; this helper does not implement general inventory policy.
use super::{db_err, CallError};
use super::inventory::notify_item_change;
use crate::consts::{FLAG_HANGAR, IX_FLAG, IX_LOCATION_ID, IX_STACK_SIZE, LOCATION_JUNKYARD};
use crate::dao::items::{inventory, transfers};
use crate::dao::items::loading::{load_items, Item, ItemQuery};
use crate::db::{executor::Executor, outbox::Outbox};
use marshal::Value;

pub(super) async fn take_items<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, item: Item, quantity: i32) -> Result<(), CallError> {
    let owner = item.owner_id;
    if quantity >= item.quantity {
        inventory::delete(db, item.item_id).await.map_err(db_err)?;
        let old = item.location_id;
        let gone = Item { location_id: LOCATION_JUNKYARD, ..item };
        outbox.notify_pilot(owner, "OnItemChange", vec![gone.row(), Value::Dict(vec![(Value::Int(IX_LOCATION_ID), Value::Int(old))]), Value::None]);
    } else {
        transfers::take(db, item.item_id, quantity).await.map_err(db_err)?;
        let left = Item { quantity: item.quantity - quantity, ..item };
        let change = Value::Dict(vec![(Value::Int(IX_STACK_SIZE), Value::Int((left.quantity + quantity) as i64))]);
        outbox.notify_pilot(owner, "OnItemChange", vec![left.row(), change, Value::None]);
    }
    Ok(())
}
pub(super) async fn deliver<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, owner: i64, station: i64, type_id: i32, quantity: i32) -> Result<(), CallError> {
    let stack = transfers::stack(db, station, FLAG_HANGAR, type_id, owner).await.map_err(db_err)?;
    let (item_id, change) = match stack {
        Some(r) => { transfers::add(db, r.item_id, quantity).await.map_err(db_err)?; (r.item_id, (IX_STACK_SIZE, r.quantity as i64)) }
        None => { let id = transfers::insert(db, type_id, owner, station, FLAG_HANGAR, quantity).await.map_err(db_err)?; (id, (IX_LOCATION_ID, 0)) }
    };
    let docked = transfers::docked(db, owner).await.map_err(db_err)?;
    if docked == Some(station) {
        if let Some(item) = load_items(db, ItemQuery::Id(item_id)).await.map_err(db_err)?.pop() {
            let change = Value::Dict(vec![(Value::Int(change.0), Value::Int(change.1))]);
            outbox.notify_pilot(owner, "OnItemChange", vec![item.row(), change, Value::None]);
        }
    }
    Ok(())
}
pub(super) async fn deposit<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, owner: i64, location: i64, flag: i32, type_id: i32, quantity: i32, from: i64) -> Result<(), CallError> {
    match transfers::stack(db, location, flag, type_id, owner).await.map_err(db_err)? {
        Some(r) => { transfers::add(db, r.item_id, quantity).await.map_err(db_err)?; notify_item_change(db, outbox, r.item_id, vec![(IX_STACK_SIZE, Value::Int(r.quantity as i64))]).await }
        None => { let id = transfers::insert(db, type_id, owner, location, flag, quantity).await.map_err(db_err)?; notify_item_change(db, outbox, id, vec![(IX_LOCATION_ID, Value::Int(from)), (IX_FLAG, Value::Int(flag as i64))]).await }
    }
}
