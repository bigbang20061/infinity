//! Item movement and authorization for invInventory (eve/client/script/environment/invCache.py).
//! Moves require an owned source, an authorized corporation office, or a nearby
//! positioned wreck or cargo container, and an owned or authorized destination.
//! Successful inventory moves queue item-move events after their writes.
//! Departing online modules are offlined transactionally; bank and cycle effects
//! are dispatched after commit and running cycles finish normally.
//! Loot rights and suspect flags are not implemented.

use super::*;
use crate::consts::{CORP_HANGAR_DIVISIONS, IX_FLAG, IX_SINGLETON, IX_OWNER_ID};

const FLAG_AUTOFIT: i32 = 0;
/// appConst.py:1475, maxCargoContainerTransferDistance, in metres.
const MAX_CARGO_CONTAINER_TRANSFER_DISTANCE: f64 = 2500.0;

pub(super) enum MoveEffect {
    Bank(super::super::combat::banks::BankStop),
    Online(super::super::effects::online::OnlineChanged),
}

impl From<super::super::effects::online::OnlineChanged> for MoveEffect {
    fn from(change: super::super::effects::online::OnlineChanged) -> Self { Self::Online(change) }
}

pub(super) fn dispatch_move<T>(value: T, outbox: Outbox<MoveEffect>, c: &Client, target: i64) -> Result<T, CallError> {
    use crate::db::outbox::Effect;
    for entry in outbox.into_entries() {
        match entry {
            Effect::Operation(MoveEffect::Bank(stop)) => super::super::combat::banks::apply_stop(stop),
            Effect::Operation(MoveEffect::Online(change)) => super::super::effects::online::apply_online_changed(change),
            Effect::Notification(notification) => super::super::effects::online::dispatch_notification(gm::route_target(notification, target, c.session.int("charid")), c)?,
            Effect::Event(event) => crate::lua::on_event(event),
        }
    }
    Ok(value)
}


async fn authorized_source(c: &Client, item: &Item, source: Option<i64>, char_id: i64) -> Result<bool, CallError> {
    if source.is_some_and(|id| id != item.location_id) { return Ok(false); }
    if let Some(allowed) = custody_allowed(&c.db, &c.sde, item.item_id, char_id, c.session.int("stationid"), true).await? {
        if !allowed { return Err(notify_error(messages::NO_CORPORATION_HANGAR_TAKE_ROLE)); }
        return Ok(true);
    }
    if item.owner_id == char_id { return Ok(true); }
    let (Some(system), Some(ship)) = (c.session.int("solarsystemid"), c.session.int("shipid")) else { return Ok(false) };
    if c.session.int("stationid").is_some() { return Ok(false); }
    let Some(parent) = load_items(&c.db, &c.sde, ItemQuery::Id(item.location_id)).await.map_err(db_err)?.into_iter().next() else { return Ok(false) };
    if parent.group_id != i64::from(crate::consts::GROUP_WRECK) && !i32::try_from(parent.group_id).ok().is_some_and(|id| crate::consts::CARGO_CONTAINER_GROUPS.contains(&id)) { return Ok(false); }
    let Some(position) = crate::dao::items::social::position(&c.db, parent.item_id).await.map_err(db_err)? else { return Ok(false) };
    if position.location_id != system { return Ok(false); }
    Ok(c.world.with(system, move |s| {
        let (ship, parent) = (s.ball(ship)?, s.ball(parent.item_id)?);
        Some(crate::space::dist(ship.position_at(s.now_ms()), parent.position_at(s.now_ms()))
            - ship.radius as f64 - parent.radius as f64 <= MAX_CARGO_CONTAINER_TRANSFER_DISTANCE)
    }).await.flatten().unwrap_or(false))
}

async fn authorized_destination(c: &Client, container: &Container, flag: Option<i32>, char_id: i64) -> Result<Option<(i64, i64, i32)>, CallError> {
    if flag == Some(FLAG_CORP_DELIVERIES) { return Ok(None); }
    match container {
        Container::Deliveries { .. } => Ok(None),
        Container::Hangar { station_id, owner_id } => {
            if *owner_id != char_id || c.session.int("stationid") != Some(*station_id) { return Ok(None); }
            Ok(Some((*station_id, *owner_id, flag.filter(|f| *f != FLAG_AUTOFIT).unwrap_or(FLAG_HANGAR))))
        }
        Container::Item(id) => {
            if let Some(corp) = super::super::offices::office_owner(&c.db, *id).await? {
                if c.session.int("corpid") != Some(corp) {
                    return Err(CallError::User { msg: messages::MSG_CUSTOM_NOTIFY.into(), dict: vec![("notify".into(), Value::unicode(messages::NOT_YOUR_CORPORATION_HANGAR))] });
                }
                let sag = FLAG_CORP_SAG1;
                return Ok(Some((*id, corp, flag.filter(|f| (sag..sag + CORP_HANGAR_DIVISIONS).contains(f)).unwrap_or(sag))));
            }
            if corporate_custody(&c.db, Some(&c.sde), *id).await?.is_some() { return Ok(None); }
            let Some(item) = load_items(&c.db, &c.sde, ItemQuery::Id(*id)).await.map_err(db_err)?.into_iter().next() else { return Ok(None) };
            if item.owner_id != char_id { return Ok(None); }
            let default = if item.category_id == CATEGORY_SHIP as i64 { FLAG_CARGO } else { FLAG_HANGAR };
            Ok(Some((*id, char_id, flag.unwrap_or(default))))
        }
    }
}

/// Move `item_id` (all of it, or `qty` split off a stack) into `container` under `flag`.
/// Returns the ID of the item now at the destination.
pub(crate) async fn move_item(c: &mut Client, container: &Container, item_id: i64, source: Option<i64>, flag: Option<i32>, qty: Option<i64>) -> Result<Option<i64>, CallError> {
    let _lock = super::super::market::lock().await;
    let char_id = c.session.int("charid").unwrap_or(0);
    let Some(item) = load_items(&c.db, &c.sde, ItemQuery::Id(item_id)).await.map_err(db_err)?.into_iter().next() else { return Ok(None) };
    let allowed_source = authorized_source(c, &item, source, char_id).await?;
    let destination = if allowed_source { authorized_destination(c, container, flag, char_id).await? } else { None };
    let Some((location, owner, flag)) = destination else {
        let destination = match container { Container::Hangar { station_id, .. } | Container::Deliveries { station_id, .. } => *station_id, Container::Item(id) => *id };
        warn!(char_id, item_id, destination, "inventory move authorization refused");
        return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
    };
    if Some(item_id) == c.session.int("shipid") && matches!(container, Container::Item(_)) {
        return Err(CallError::User { msg: messages::MSG_CAN_NOT_PLACE_ACTIVE_SHIP_IN_ANOTHER_HANGAR.into(), dict: vec![] });
    }
    let db = c.db.clone();
    let sde = c.sde.clone();
    let active_ship = c.session.int("shipid");
    let station = c.session.int("stationid");
    let policy = super::super::effects::online::OnlinePolicy::from_client(c)?;
    db.transaction(
        move |tx, outbox: &mut Outbox<MoveEffect>| {
            let item = item.clone();
            let sde = sde.clone();
            let policy = policy.clone();
            Box::pin(async move {
                if custody_allowed(tx, &sde, item.item_id, char_id, station, true).await? == Some(false) {
                    return Err(notify_error(messages::NO_CORPORATION_HANGAR_TAKE_ROLE));
                }
                let destination_fitting = is_fitting_flag(flag) || (flag == FLAG_AUTOFIT
                    && (item.category_id == i64::from(crate::consts::CATEGORY_MODULE) || item.category_id == i64::from(crate::consts::CATEGORY_SUBSYSTEM)));
                let source_fitting = is_fitting_flag(item.flag_id);
                let mut locks = Vec::new();
                if source_fitting { locks.push(item.location_id); }
                if destination_fitting { locks.push(location); }
                locks.sort_unstable();
                locks.dedup();
                for ship in locks {
                    let exists = crate::dao::items::ship::lock_reload_ship(tx, ship).await.map_err(db_err)?;
                    if ship == location && destination_fitting && !exists {
                        return Err(notify_error(messages::INSUFFICIENT_ITEM_QUANTITY));
                    }
                }
                if destination_fitting {
                    let hull = load_items(tx, &sde, ItemQuery::Id(location)).await.map_err(db_err)?.into_iter().next().ok_or_else(|| notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
                    if hull.owner_id != owner || hull.category_id != i64::from(CATEGORY_SHIP) || !hull.singleton
                        || (active_ship != Some(location) && !(station == Some(hull.location_id) && hull.flag_id == FLAG_HANGAR)) {
                        return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
                    }
                }
                let flag = if flag == FLAG_AUTOFIT && destination_fitting {
                    super::super::effects::fitting::free_slot(tx, sde.clone(), owner, location, item.item_id, item.type_id).await?
                        .ok_or_else(|| notify_error(messages::fitting::SLOTS))?
                } else if flag == FLAG_AUTOFIT { FLAG_CARGO } else { flag };
                let take = if is_fitting_flag(flag) { Some(1) } else { qty }
                    .filter(|q| *q > 0 && !item.singleton && (*q as i32) < item.quantity);
                let departure = source_fitting && take.is_none() && (item.location_id != location || item.flag_id != flag || item.owner_id != owner);
                let leaving_fit = departure && (item.location_id != location || !is_fitting_flag(flag) || item.owner_id != owner);
                let online = leaving_fit && item.category_id == i64::from(crate::consts::CATEGORY_MODULE)
                    && crate::dao::items::dogma::online(tx, item.location_id).await.map_err(db_err)?.iter().any(|entry| entry.item_id == item.item_id);
                if online {
                    super::super::effects::online::set_online_core(tx, &sde, &policy, outbox, item.location_id, item.item_id, false).await?;
                }
                let before = if departure && !online { Some(super::super::combat::banks::snapshot(tx, item.location_id, &[item.item_id]).await?) } else { None };
                let master = if departure && !online { super::super::combat::targeting_charges::master_for_member(tx, item.location_id, item.item_id).await? } else { item.item_id };
                let moved_id = move_core(tx, &sde, outbox, &item, owner, location, flag, take).await?;
                if is_fitting_flag(flag) {
                    super::super::effects::fitting::validate_fit(tx, sde.clone(), owner, location, moved_id).await?;
                }
                if let Some(before) = before {
                    if super::super::combat::banks::remove_member(tx, item.location_id, item.item_id, master, false).await? {
                        for stop in super::super::combat::banks::changes(tx, item.location_id, before).await? { outbox.operation(MoveEffect::Bank(stop)); }
                        outbox.notify_client("OnWeaponBanksChanged", vec![Value::Int(item.location_id), super::super::combat::banks(tx, item.location_id).await?]);
                    }
                }
                if departure { super::super::effects::online::reconcile_core(tx, &sde, outbox, item.owner_id, item.location_id).await?; }
                if is_fitting_flag(flag) { super::super::effects::online::reconcile_core(tx, &sde, outbox, owner, location).await?; }
                if take.is_some() || item.location_id != location || item.flag_id != flag || item.owner_id != owner || (is_fitting_flag(flag) && !item.singleton) {
                    outbox.event(crate::lua::events::GameEvent::ItemsMoved {
                        char_id, item_id: moved_id, from_location_id: item.location_id,
                        to_location_id: location, quantity: take.map_or(item.quantity, |q| q as i32),
                    });
                }
                Ok(Some(moved_id))
            })
        },
        |value, outbox| async move { dispatch_move(value, outbox, c, char_id) },
    ).await
}

pub(super) async fn move_core<X: Send>(
    tx: &(impl Executor + ?Sized), sde: &Sde, outbox: &mut Outbox<X>,
    item: &Item, owner: i64, location: i64, flag: i32, take: Option<i64>,
) -> Result<i64, CallError> {
    if !crate::dao::trades::reserved(tx, &[item.item_id, location]).await.map_err(db_err)?.is_empty() {
        return Err(notify_error(messages::trade::TRADE_ITEM_RESERVED));
    }
    if crate::dao::industry::has_active_blueprint_job(tx, &[item.item_id]).await.map_err(db_err)? {
        return Err(notify_error(messages::industry::BLUEPRINT_ACTIVE));
    }
    let item_id = item.item_id;
    let fitting = is_fitting_flag(flag);
    let corporate = corporate_custody(tx, Some(sde), item.item_id).await?.is_some();
    if let Some(q) = take {
        let id = split_stack(tx, item, q as i32, owner, location, flag, fitting).await?;
        if item.owner_id != owner && corporate {
            let left = Item { quantity: item.quantity - q as i32, ..item.clone() };
            notify_corp_item_change(tx, outbox, &left, vec![(IX_STACK_SIZE, Value::Int(item.quantity as i64))]).await?;
        } else {
            notify_item_change(tx, sde, outbox, item_id, vec![(IX_STACK_SIZE, Value::Int(item.quantity as i64))]).await?;
        }
        let mut change = vec![(IX_LOCATION_ID, Value::Int(item.location_id)), (IX_FLAG, Value::Int(item.flag_id as i64))];
        if item.owner_id != owner { change.push((IX_OWNER_ID, Value::Int(item.owner_id))); }
        notify_item_change(tx, sde, outbox, id, change).await?;
        Ok(id)
    } else {
        if item.owner_id != owner && corporate {
            let gone = Item { location_id: LOCATION_JUNKYARD, ..item.clone() };
            notify_corp_item_change(tx, outbox, &gone, vec![(IX_LOCATION_ID, Value::Int(item.location_id))]).await?;
        }
        let singleton = item.singleton || fitting;
        if !inventory_dao::move_item(tx, item, location, flag, owner, singleton).await.map_err(db_err)? {
            return Err(notify_error(messages::INSUFFICIENT_ITEM_QUANTITY));
        }
        if item.owner_id != owner && corporate {
            crate::dao::items::transfers::hand_over(tx, &[crate::dao::items::transfers::Transfer { item_id, owner, location, flag }]).await.map_err(db_err)?;
        }
        let mut change = vec![(IX_LOCATION_ID, Value::Int(item.location_id)), (IX_FLAG, Value::Int(item.flag_id as i64))];
        if item.owner_id != owner { change.push((IX_OWNER_ID, Value::Int(item.owner_id))); }
        if singleton != item.singleton {
            change.push((IX_SINGLETON, Value::Bool(item.singleton)));
        }
        notify_item_change(tx, sde, outbox, item_id, change).await?;
        Ok(item_id)
    }
}
