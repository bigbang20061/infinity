//! Weapon bank RPC policy mirrored from `eve/client/script/dogma/clientDogmaLocation.py`.
//! Membership, charges and pending reloads come from `game.weapon_banks`,
//! `game.module_charges` and `game.pending_reloads`; fitting and online state use items.
//! Requested changes return their map without notifications. Offline/unfit reconciliation,
//! cycle stopping and slave activation redirection are not implemented here.
use super::*;
use crate::consts::{CATEGORY_MODULE, CATEGORY_SHIP, GROUPABLE_GROUP_IDS};
use crate::dao::items::{charges, combat, dogma, pending_reloads, ship as ship_dao};

#[derive(Clone)]
struct Module {
    item: Item,
    online: bool,
    charge: Option<(i32, i64)>,
}

fn effective_charge(current: Option<(i32, i32)>, pending: Option<(i32, i32)>) -> Option<(i32, i64)> {
    let current = current.filter(|(_, quantity)| *quantity > 0);
    match pending {
        Some((kind, reserved)) => {
            let have = current.filter(|(current_kind, _)| *current_kind == kind).map_or(0, |(_, quantity)| i64::from(quantity));
            let quantity = have + i64::from(reserved);
            (quantity > 0).then_some((kind, quantity))
        }
        None => current.map(|(kind, quantity)| (kind, i64::from(quantity))),
    }
}

fn validate_pair(a: &Module, b: &Module) -> Result<(), CallError> {
    if !a.online || !b.online { return Err(user_error(messages::MSG_CANT_LINK_MODULE_NOT_ONLINE, vec![])); }
    if a.item.type_id != b.item.type_id { return Err(notify_error(messages::combat::TYPE_MISMATCH)); }
    if !GROUPABLE_GROUP_IDS.contains(&a.item.group_id) || !GROUPABLE_GROUP_IDS.contains(&b.item.group_id) {
        return Err(notify_error(messages::combat::NOT_GROUPABLE));
    }
    if a.charge != b.charge { return Err(notify_error(messages::combat::CHARGE_MISMATCH)); }
    Ok(())
}

async fn modules(ex: &(impl Executor + ?Sized), sde: &Sde, ship: i64, owner: i64) -> Result<Vec<Module>, CallError> {
    let mut items = load_items(ex, sde, ItemQuery::LocationOwner(ship, owner)).await.map_err(db_err)?;
    items.retain(|i| i.singleton && i.category_id == i64::from(CATEGORY_MODULE) && HI_SLOTS.contains(&i.flag_id));
    items.sort_unstable_by_key(|i| (i.flag_id, i.item_id));
    let online = dogma::online(ex, ship).await.map_err(db_err)?;
    let charges = charges::all(ex, ship).await.map_err(db_err)?;
    let flags: Vec<_> = items.iter().map(|i| i.flag_id).collect();
    let pending = pending_reloads::for_flags(ex, ship, &flags).await.map_err(db_err)?;
    Ok(items.into_iter().map(|item| {
        let current = charges.iter().find(|c| c.flag_id == item.flag_id).map(|c| (c.type_id, c.quantity));
        let reload = pending.iter().rev().find(|r| r.flag_id == item.flag_id && r.module_id == item.item_id).map(|r| (r.type_id, r.quantity));
        let online = online.iter().any(|r| r.item_id == item.item_id);
        Module { item, online, charge: effective_charge(current, reload) }
    }).collect())
}

fn module(items: &[Module], id: i64) -> Result<&Module, CallError> {
    items.iter().find(|m| m.item.item_id == id).ok_or_else(|| notify_error(messages::combat::INVALID_MODULE))
}

fn grouped(items: &[Module], rows: &[charges::Bank]) -> Result<Vec<(i64, Vec<i64>)>, CallError> {
    let mut groups: BTreeMap<(i32, Option<(i32, i64)>), Vec<&Module>> = BTreeMap::new();
    for m in items.iter().filter(|m| GROUPABLE_GROUP_IDS.contains(&m.item.group_id)) {
        if !m.online { return Err(user_error(messages::MSG_CANT_LINK_MODULE_NOT_ONLINE, vec![])); }
        groups.entry((m.item.type_id, m.charge)).or_default().push(m);
    }
    Ok(groups.into_values().filter(|group| group.len() > 1).map(|group| {
        let master = group.iter().find(|m| rows.iter().any(|r| r.master_id == m.item.item_id))
            .unwrap_or(&group[0]).item.item_id;
        (master, group.iter().filter_map(|m| (m.item.item_id != master).then_some(m.item.item_id)).collect())
    }).collect())
}

async fn operation(ex: &(impl Executor + ?Sized), sde: &Sde, ship: i64, owner: i64, method: &str, first: i64, second: i64) -> CallResult {
    if !ship_dao::lock_reload_ship(ex, ship).await.map_err(db_err)? { return Err(notify_error(messages::combat::NOT_YOUR_SHIP)); }
    let hull = load_items(ex, sde, ItemQuery::Id(ship)).await.map_err(db_err)?.into_iter().next();
    if !hull.is_some_and(|h| h.owner_id == owner && h.category_id == i64::from(CATEGORY_SHIP)) {
        return Err(notify_error(messages::combat::NOT_YOUR_SHIP));
    }
    match method {
        "GetWeaponBankInfoForShip" => banks(ex, ship).await,
        "UnlinkAllModules" => { combat::clear_banks(ex, ship).await.map_err(db_err)?; Ok(Value::None) }
        "DestroyWeaponBank" => {
            let master = charges::bank_of(ex, ship, first).await.map_err(db_err)?;
            combat::destroy(ex, ship, master).await.map_err(db_err)?;
            Ok(Value::None)
        }
        "UnlinkModule" => {
            let master = charges::bank_of(ex, ship, first).await.map_err(db_err)?;
            let peeled = combat::peel_first(ex, ship, master).await.map_err(db_err)?
                .ok_or_else(|| notify_error(messages::combat::BANK_NOT_FOUND))?;
            Ok(Value::Int(peeled))
        }
        "LinkAllWeapons" => {
            let items = modules(ex, sde, ship, owner).await?;
            let rows = charges::banks(ex, ship).await.map_err(db_err)?;
            let groups = grouped(&items, &rows)?;
            combat::clear_banks(ex, ship).await.map_err(db_err)?;
            for (master, slaves) in groups { for slave in slaves { combat::move_bank(ex, ship, master, slave).await.map_err(db_err)?; } }
            banks(ex, ship).await
        }
        "LinkWeapons" | "MergeModuleGroups" | "PeelAndLink" => {
            let target = charges::bank_of(ex, ship, first).await.map_err(db_err)?;
            let source = charges::bank_of(ex, ship, second).await.map_err(db_err)?;
            if target == source { return banks(ex, ship).await; }
            let items = modules(ex, sde, ship, owner).await?;
            let a = module(&items, target)?;
            let slaves = charges::slaves(ex, ship, source).await.map_err(db_err)?;
            if method == "PeelAndLink" {
                let Some(slave) = slaves.first() else { return banks(ex, ship).await; };
                validate_pair(a, module(&items, slave.slave_id)?)?;
                combat::remove_slave(ex, ship, slave.slave_id).await.map_err(db_err)?;
                combat::move_bank(ex, ship, target, slave.slave_id).await.map_err(db_err)?;
            } else if method == "LinkWeapons" && source != second {
                validate_pair(a, module(&items, second)?)?;
                combat::remove_slave(ex, ship, second).await.map_err(db_err)?;
                combat::move_bank(ex, ship, target, second).await.map_err(db_err)?;
            } else {
                validate_pair(a, module(&items, source)?)?;
                for slave in slaves { validate_pair(a, module(&items, slave.slave_id)?)?; }
                combat::move_bank(ex, ship, target, source).await.map_err(db_err)?;
            }
            banks(ex, ship).await
        }
        _ => unreachable!(),
    }
}

fn authorize_ship(requested: Option<i64>, active: Option<i64>) -> Result<i64, CallError> {
    requested.filter(|ship| Some(*ship) == active).ok_or_else(|| notify_error(messages::combat::NOT_YOUR_SHIP))
}

pub(super) async fn weapon_groups(c: &mut Client, call: &Call<'_>) -> CallResult {
    let ship = authorize_ship(call.args.first().and_then(|v| v.as_int()), c.session.int("shipid"))?;
    let owner = c.session.int("charid").ok_or_else(|| notify_error(messages::combat::NOT_YOUR_SHIP))?;
    let first = call.args.get(1).and_then(|v| v.as_int()).unwrap_or_default();
    let second = call.args.get(2).and_then(|v| v.as_int()).unwrap_or_default();
    let method = call.method.to_owned();
    let sde = c.sde.clone();
    let db = c.db.clone();
    db.transaction(|tx, _: &mut Outbox| {
        let sde = sde.clone();
        let method = method.clone();
        Box::pin(async move { operation(tx, &sde, ship, owner, &method, first, second).await })
    }, |value, outbox| async move { dispatch_client(value, outbox, c) }).await
}

#[cfg(test)] mod tests;
