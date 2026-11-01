//! Contract lifecycle mechanics for contractProxy, mirroring eve/client/script/ui/shared/neocom/contracts.
//! Uses game contracts and items through database access and immutable SDE types and stations; corporation contracts are unsupported.
//! Requested exchange items retain their identity, state and contents when handed over.
//! Expiry, courier duration, item counts and text lengths use the configured contracts policy.

use super::*;
use crate::dao::contracts::lifecycle as contract_dao;

/// `CreateContract(ContractStruct, confirm)` -> [contractID]: one contract, or one per item
/// for a multi-contract (fitted ships).
pub(super) async fn create(c: &Client, info: &Value) -> CallResult {
    let (db, world) = (&c.db, &*c.world);
    let sde = Arc::clone(&c.sde);
    let me = c.session.int("charid").unwrap_or(0);
    let corp = c.session.int("corpid").unwrap_or(0);
    let int = |k: &str| field(info, k).and_then(|v| v.as_int()).unwrap_or(0);
    let money = |k: &str| round2(field(info, k).and_then(|v| v.as_float()).unwrap_or(0.0));
    let text = |k: &str, max: usize| value_text(field(info, k), max);
    if truthy(field(info, "forCorp")) {
        return Err(notify_error(messages::CORPORATION_CONTRACTS_UNSUPPORTED));
    }
    let kind = int("contractType") as i32;
    if ![TYPE_ITEM_EXCHANGE, TYPE_AUCTION, TYPE_COURIER].contains(&kind) {
        return Err(notify_error(messages::CONTRACT_TYPE_REQUIRED));
    }
    let (price, reward, collateral) = (money("price"), money("reward"), money("collateral"));
    if price < 0.0 || reward < 0.0 || collateral < 0.0 {
        return Err(notify_error(messages::CONTRACT_AMOUNT_NEGATIVE));
    }
    let start = station(&c.sde, int("startStationID"))?;
    let end = if kind == TYPE_COURIER { Some(station(&c.sde, int("destinationID"))?) } else { None };
    let assignee = int("assignedToID");
    if assignee == me {
        return Err(notify_error(messages::CONTRACT_SELF_ASSIGNMENT_FORBIDDEN));
    }
    let policy = &crate::config::game().contracts;
    let minutes = i128::from(int("minutesExpire")).clamp(
        i128::from(policy.expiry_min_minutes),
        i128::from(policy.expiry_max_days) * i128::from(FILETIME_DAY / FILETIME_MINUTE),
    );
    let num_days = if kind == TYPE_COURIER {
        i32::try_from(int("numDays").clamp(policy.courier_min_days, policy.courier_max_days)).unwrap_or(i32::MAX)
    } else { 0 };
    let (title, description) = (
        text("title", usize::try_from(policy.title_max_len).unwrap_or(usize::MAX)),
        text("description", usize::try_from(policy.description_max_len).unwrap_or(usize::MAX)),
    );
    let items: Vec<(i64, i64)> = field(info, "itemList")
        .and_then(|v| v.items())
        .unwrap_or(&[])
        .iter()
        .filter_map(|e| match e.items()? {
            [id, qty, ..] => Some((id.as_int()?, qty.as_int().unwrap_or(1))),
            _ => None,
        })
        .collect();
    let mut requested = Vec::new();
    if kind == TYPE_ITEM_EXCHANGE {
        if let Some(value) = field(info, "requestItemTypeList") {
            let entries = value.items().ok_or_else(|| notify_error(messages::CONTRACT_REQUIRED_ITEMS_MISSING))?;
            for entry in entries {
                let (type_id, qty) = match entry.items() {
                    Some([t, q, ..]) => (t.as_int(), q.as_int()),
                    _ => (None, None),
                };
                let type_id = type_id.and_then(|v| i32::try_from(v).ok()).filter(|v| *v > 0)
                    .ok_or_else(|| notify_error(messages::CONTRACT_REQUIRED_ITEMS_MISSING))?;
                let qty = qty.and_then(|v| i32::try_from(v).ok()).filter(|v| *v > 0)
                    .ok_or_else(|| notify_error(messages::CONTRACT_REQUIRED_ITEMS_MISSING))?;
                if sde.types.get(type_id).is_none() || requested.iter().any(|(t, _)| *t == type_id) {
                    return Err(notify_error(messages::CONTRACT_REQUIRED_ITEMS_MISSING));
                }
                requested.push((type_id, qty));
            }
        }
    }
    match kind {
        TYPE_AUCTION if items.is_empty() => return Err(notify_error(messages::AUCTION_REQUIRES_ITEMS)),
        TYPE_AUCTION if collateral > 0.0 && collateral < price => return Err(notify_error(messages::BUYOUT_BELOW_START_PRICE)),
        TYPE_COURIER if items.is_empty() => return Err(notify_error(messages::COURIER_REQUIRES_ITEMS)),
        TYPE_ITEM_EXCHANGE if items.is_empty() && requested.is_empty() => return Err(notify_error(messages::ITEM_EXCHANGE_REQUIRES_OFFER_OR_REQUEST)),
        _ => {}
    }
    if items.len() as u128 > policy.max_items as u128 || requested.len() as u128 > policy.max_items as u128 {
        return Err(notify_error(messages::CONTRACT_ITEM_LIMIT_EXCEEDED));
    }
    let multi = truthy(field(info, "multiContract")) && items.len() > 1;
    let groups: Vec<Vec<(i64, i64)>> = if multi { items.iter().map(|i| vec![*i]).collect() } else { vec![items] };

    let _trading = lock().await;
    let now = filetime_now();
    let start_id = start.id;
    let end_id = end.map(|s| s.id).unwrap_or(0);
    let input = Arc::new((groups, requested, title, description));
    let created = db.transaction(
        |db, outbox: &mut Outbox| {
            let input = Arc::clone(&input);
            let sde = Arc::clone(&sde);
            Box::pin(async move {
    let (groups, requested, title, description) = &*input;
    let open = num_outstanding(db, me).await?;
    let cap = max_outstanding(db, me).await?;
    if open + groups.len() as i64 > cap {
        return Err(notify_error(&messages::contract_limit(open, cap)));
    }
    // Every item must be in the issuer's hangar at the start station.
    let mut loaded = HashMap::new();
    for (id, qty) in groups.iter().flatten() {
        let item = load_items(db, &sde, ItemQuery::Id(*id)).await.map_err(db_err)?.pop().ok_or_else(|| notify_error(messages::CONTRACT_ITEM_MISSING))?;
        if item.owner_id != me || item.location_id != start_id || item.flag_id != FLAG_HANGAR {
            return Err(notify_error(messages::CONTRACT_ITEM_MUST_BE_IN_START_HANGAR));
        }
        if item.type_id == TYPE_PLASTIC_WRAP && kind != TYPE_COURIER {
            return Err(notify_error(messages::COURIER_PACKAGE_NOT_TRADEABLE));
        }
        if *qty < 1 || (!item.singleton && *qty > item.quantity as i64) {
            return Err(notify_error(messages::CONTRACT_ITEM_QUANTITY_INSUFFICIENT));
        }
        loaded.insert(*id, item);
    }
    let escrow_isk = if kind == TYPE_AUCTION { 0.0 } else { reward };
    let mut created = vec![];
    for group in groups {
        debit(db, outbox, me, escrow_isk).await?;
        let id = contract_dao::insert(db, &contract_dao::NewContract {
            kind, issuer: me, issuer_corp: corp, assignee, start: start_id, end: end_id,
            price, reward, collateral, title, description, num_days,
            status: STATUS_OUTSTANDING, issued: now, expires: expiry_time(now, minutes),
        }).await.map_err(db_err)?;
        for (item_id, qty) in group {
            if let Some(item) = loaded.remove(item_id) {
                escrow_item(db, outbox, item, *qty as i32, id).await?;
            }
        }
        for (type_id, qty) in requested {
            contract_dao::insert_requested_item(db, id, *type_id, *qty).await.map_err(db_err)?;
        }
        let volume = volume(db, &sde, id).await?;
        contract_dao::update_volume(db, id, volume).await.map_err(db_err)?;
        tell(outbox, if assignee == corp { 0 } else { assignee }, "OnContractAssigned", id);
        created.push(Value::Int(id));
    }
    outbox.notify_pilot(me, "OnContractCreated", vec![]);
    Ok(created)
            })
        },
        |value, outbox| async move { dispatch_world(value, outbox, world) },
    ).await?;
    for id in &created {
        info!("contracts: {me} created contract {id:?} (type {kind}) at {start_id}");
    }
    Ok(Value::List(created))
}

fn expiry_time(now: i64, minutes: i128) -> i64 {
    (i128::from(now) + minutes * i128::from(FILETIME_MINUTE)).min(i128::from(i64::MAX)) as i64
}

async fn volume(db: &(impl crate::db::executor::Executor + ?Sized), sde: &crate::sde::Sde, id: i64) -> Result<f64, CallError> {
    Ok(sum_volume(shared::items(db, &[id]).await.map_err(db_err)?.iter()
        .filter(|item| item.in_crate)
        .filter_map(|item| sde.types.get(item.type_id as i32).and_then(|t| t.volume).map(|volume| volume * item.quantity as f64))))
}

fn sum_volume(volumes: impl Iterator<Item = f64>) -> f64 {
    // Match SQL COALESCE(SUM, 0) sign on an empty set.
    volumes.fold(0.0, |total, v| total + v)
}

/// `AcceptContract(contractID, forCorp)` -> the contract.
pub(super) async fn accept(c: &Client, id: i64) -> CallResult {
    let (db, world) = (&c.db, &*c.world);
    let me = c.session.int("charid").unwrap_or(0);
    let corp = c.session.int("corpid").unwrap_or(0);
    let _trading = lock().await;
    let now = filetime_now();
    let sde = Arc::clone(&c.sde);
    db.transaction(
        |db, outbox: &mut Outbox| {
            let sde = Arc::clone(&sde);
            Box::pin(async move {
            let mut k = contract(db, id).await?;
            if k.status != STATUS_OUTSTANDING || k.expired(now) {
                return Err(user_error(messages::MSG_CON_CONTRACT_NOT_OUTSTANDING, vec![]));
            }
            if k.issuer == me {
                return Err(notify_error(messages::CONTRACT_SELF_ACCEPTANCE_FORBIDDEN));
            }
            if !k.available_to(me, corp) {
                return Err(notify_error(messages::CONTRACT_ASSIGNED_TO_OTHER));
            }
            match k.kind {
                TYPE_ITEM_EXCHANGE => {
                    // The requested items must all be in the acceptor's hangar at the start station.
                    let wanted = contract_dao::requested_items(db, id).await.map_err(db_err)?;
                    let mut takes = vec![];
                    let mut types = std::collections::HashSet::new();
                    for w in &wanted {
                        let (type_id, qty) = (w.type_id, w.quantity);
                        if type_id <= 0 || qty <= 0 || !types.insert(type_id) || sde.types.get(type_id).is_none() {
                            return Err(notify_error(messages::CONTRACT_REQUIRED_ITEMS_MISSING));
                        }
                        let stacks = load_items(db, &sde, ItemQuery::OwnerLocationFlagTypeDescending(me, k.start, FLAG_HANGAR, type_id)).await.map_err(db_err)?;
                        let have: i64 = stacks.iter().map(|s| if s.singleton { 1 } else { i64::from(s.quantity.max(0)) }).sum();
                        if have < i64::from(qty) {
                            return Err(notify_error(messages::CONTRACT_REQUIRED_ITEMS_MISSING));
                        }
                        let mut left = qty;
                        for item in stacks {
                            if left == 0 { break; }
                            let n = if item.singleton { 1 } else { item.quantity.min(left) };
                            if n <= 0 { continue; }
                            left -= n;
                            takes.push((item, n));
                        }
                    }
                    takes.sort_unstable_by_key(|(item, _)| item.item_id);
                    let ids: Vec<_> = takes.iter().map(|(item, _)| item.item_id).collect();
                    let tree = crate::dao::trades::descendants(db, &ids).await.map_err(db_err)?;
                    if !crate::dao::trades::reserved(db, &tree).await.map_err(db_err)?.is_empty() {
                        return Err(notify_error(messages::trade::TRADE_ITEM_RESERVED));
                    }
                    debit(db, outbox, me, k.price).await?;
                    let mut plan = Vec::new();
                    let mut full_ids = std::collections::HashSet::new();
                    for (item, quantity) in takes {
                        let item_id = if !item.singleton && quantity < item.quantity {
                            let id = super::super::inventory::split_stack(db, &item, quantity, me, k.start, FLAG_HANGAR, false).await?;
                            let remaining = Item { quantity: item.quantity - quantity, ..item.clone() };
                            outbox.notify_pilot(me, "OnItemChange", vec![remaining.row(), Value::Dict(vec![(Value::Int(IX_STACK_SIZE), Value::Int(i64::from(item.quantity)))]), Value::None]);
                            id
                        } else {
                            if !crate::dao::items::inventory::move_item(db, &item, item.location_id, item.flag_id, item.owner_id, item.singleton).await.map_err(db_err)? {
                                return Err(notify_error(messages::CONTRACT_REQUIRED_ITEMS_MISSING));
                            }
                            full_ids.insert(item.item_id);
                            item.item_id
                        };
                        plan.push(transfers::Transfer { item_id, owner: k.issuer, location: k.start, flag: FLAG_HANGAR });
                    }
                    transfers::hand_over(db, &plan).await.map_err(db_err)?;
                    let ids: Vec<_> = plan.iter().map(|transfer| transfer.item_id).collect();
                    let docked = transfers::docked(db, k.issuer).await.map_err(db_err)? == Some(k.start);
                    for item in load_items(db, &sde, ItemQuery::Ids(&ids)).await.map_err(db_err)? {
                        let change = Value::Dict(vec![(Value::Int(IX_OWNER_ID), Value::Int(me))]);
                        if full_ids.contains(&item.item_id) {
                            outbox.notify_pilot(me, "OnItemChange", vec![item.row(), change.clone(), Value::None]);
                        }
                        if docked {
                            outbox.notify_pilot(k.issuer, "OnItemChange", vec![item.row(), change, Value::None]);
                        }
                    }
                    credit(db, outbox, k.issuer, k.price).await?;
                    credit(db, outbox, me, k.reward).await?;
                    hand_over(db, &sde, outbox, id, me, k.start, FLAG_HANGAR).await?;
                    k.acceptor = me;
                    k.accepted = now;
                    contract_dao::set_date_accepted(db, id, now).await.map_err(db_err)?;
                    set_status(db, &mut k, STATUS_FINISHED, now).await?;
                    tell(outbox, k.issuer, "OnContractCompleted", id);
                }
                TYPE_COURIER => {
                    debit(db, outbox, me, k.collateral).await?;
                    let wrap = contract_dao::insert_package(db, TYPE_PLASTIC_WRAP, me, k.start, FLAG_HANGAR, &messages::contract_package_name(id)).await.map_err(db_err)?;
                    show_items(db, &sde, outbox, me, k.start, &[wrap], LOCATION_JUNKYARD).await?;
                    k.acceptor = me;
                    k.accepted = now;
                    k.status = STATUS_IN_PROGRESS;
                    k.package = Some(wrap);
                    contract_dao::accept_courier(db, id, me, now, STATUS_IN_PROGRESS, wrap).await.map_err(db_err)?;
                    tell(outbox, k.issuer, "OnContractAccepted", id);
                }
                _ => return Err(notify_error(messages::AUCTION_ONLY_BIDDABLE)),
            }
            Ok(k.keyval(&sde))
            })
        },
        |value, outbox| async move { dispatch_world(value, outbox, world) },
    ).await.map(|value| {
        info!("contracts: {me} accepted contract {id}");
        value
    })
}

/// `CompleteContract(contractID, status)`: deliver a courier package (finished), give one up
/// or call an overdue one in (failed), or turn down a contract assigned to you (rejected).
pub(super) async fn complete(c: &Client, id: i64, status: i32) -> CallResult {
    let (db, world) = (&c.db, &*c.world);
    let sde = &c.sde;
    let me = c.session.int("charid").unwrap_or(0);
    let corp = c.session.int("corpid").unwrap_or(0);
    let _trading = lock().await;
    let now = filetime_now();
    db.transaction(
        |db, outbox: &mut Outbox| {
            let sde = sde.clone();
            Box::pin(async move {
            let sde = &*sde;
            let mut k = contract(db, id).await?;
            match status {
                STATUS_FINISHED => {
                    if k.kind != TYPE_COURIER || k.status != STATUS_IN_PROGRESS || k.acceptor != me {
                        return Err(notify_error(messages::COURIER_DELIVERY_REQUIRES_ACCEPTOR));
                    }
                    let wrap = match k.package {
                        Some(w) => load_items(db, sde, ItemQuery::Id(w)).await.map_err(db_err)?.pop(),
                        None => None,
                    };
                    let Some(wrap) = wrap.filter(|w| w.owner_id == me && w.location_id == k.end) else {
                        return Err(notify_error(messages::COURIER_PACKAGE_MUST_BE_AT_DESTINATION));
                    };
                    take_items(db, outbox, wrap, 1).await?;
                    hand_over(db, sde, outbox, id, k.issuer, k.end, FLAG_HANGAR).await?;
                    credit(db, outbox, me, round2(k.reward + k.collateral)).await?;
                    set_status(db, &mut k, STATUS_FINISHED, now).await?;
                    tell(outbox, k.issuer, "OnContractCompleted", id);
                }
                STATUS_FAILED => {
                    if k.kind != TYPE_COURIER || k.status != STATUS_IN_PROGRESS || !(k.acceptor == me || (k.issuer == me && k.overdue(now))) {
                        return Err(notify_error(messages::COURIER_CANNOT_BE_FAILED_NOW));
                    }
                    credit(db, outbox, k.issuer, round2(k.collateral + k.reward)).await?;
                    let wrap = match k.package {
                        Some(w) => load_items(db, sde, ItemQuery::Id(w)).await.map_err(db_err)?.pop(),
                        None => None,
                    };
                    match wrap {
                        Some(w) => {
                            let (location, flag) = (w.location_id, w.flag_id);
                            take_items(db, outbox, w, 1).await?;
                            hand_over(db, sde, outbox, id, k.acceptor, location, flag).await?;
                        }
                        None => destroy_escrow(db, id).await?,
                    }
                    set_status(db, &mut k, STATUS_FAILED, now).await?;
                    tell(outbox, if me == k.issuer { k.acceptor } else { k.issuer }, "OnContractFailed", id);
                }
                STATUS_REJECTED => {
                    if k.status != STATUS_OUTSTANDING || k.assignee == 0 || !(k.assignee == me || k.assignee == corp) {
                        return Err(notify_error(messages::CONTRACT_ASSIGNEE_REQUIRED_TO_REJECT));
                    }
                    set_status(db, &mut k, STATUS_REJECTED, now).await?;
                    tell(outbox, k.issuer, "OnContractRejected", id);
                }
                _ => return Err(notify_error(messages::CONTRACT_STATUS_TRANSITION_INVALID)),
            }
            Ok(Value::Bool(true))
        })},
        |value, outbox| async move { dispatch_world(value, outbox, world) },
    ).await.map(|value| {
        info!("contracts: {me} set contract {id} to status {status}");
        value
    })
}

/// The issuer takes an outstanding (or expired, or rejected) contract back with its escrow.
pub(super) async fn delete_one(db: &Db, sde: &Arc<crate::sde::Sde>, world: &World, me: i64, id: i64) -> Result<(), CallError> {
    let now = filetime_now();
    db.transaction(
        |db, outbox: &mut Outbox| {
            let sde = sde.clone();
            Box::pin(async move {
            let sde = &*sde;
            let mut k = contract(db, id).await?;
            if k.issuer != me || !(k.status == STATUS_OUTSTANDING || k.status == STATUS_REJECTED) {
                return Err(notify_error(messages::CONTRACT_ISSUER_REQUIRED_TO_DELETE));
            }
            if k.kind == TYPE_AUCTION && top_bid(db, id).await?.is_some() {
                return Err(notify_error(messages::BIDDED_AUCTION_CANNOT_BE_DELETED));
            }
            hand_over(db, sde, outbox, id, me, k.start, FLAG_HANGAR).await?;
            if k.kind != TYPE_AUCTION {
                credit(db, outbox, me, k.reward).await?;
            }
            set_status(db, &mut k, STATUS_DELETED, now).await?;
            Ok(())
        })},
        |value, outbox| async move { dispatch_world(value, outbox, world) },
    ).await?;
    info!("contracts: {me} deleted contract {id}");
    Ok(())
}

/// `PlaceBid(contractID, amount, forCorp)`; a bid at the buyout wins at once.
pub(super) async fn place_bid(c: &Client, id: i64, amount: f64) -> CallResult {
    let (db, world) = (&c.db, &*c.world);
    let sde = &c.sde;
    let amount = round2(amount);
    let me = c.session.int("charid").unwrap_or(0);
    let corp = c.session.int("corpid").unwrap_or(0);
    let _trading = lock().await;
    let now = filetime_now();
    let (amount, buyout) = db.transaction(
        |db, outbox: &mut Outbox| {
            let sde = sde.clone();
            Box::pin(async move {
    let sde = &*sde;
    let mut k = contract(db, id).await?;
    if k.kind != TYPE_AUCTION || k.status != STATUS_OUTSTANDING || k.expired(now) {
        return Err(user_error(messages::MSG_CON_CONTRACT_NOT_OUTSTANDING, vec![]));
    }
    if k.issuer == me {
        return Err(notify_error(messages::AUCTION_SELF_BID_FORBIDDEN));
    }
    if !k.available_to(me, corp) {
        return Err(notify_error(messages::AUCTION_ASSIGNED_TO_OTHER));
    }
    let mut amount = amount;
    let top = top_bid(db, id).await?;
    if !amount.is_finite() || amount < k.price || top.is_some_and(|(_, best)| amount <= best) {
        return Err(notify_error(messages::BID_MUST_EXCEED_CURRENT_AND_START));
    }
    let buyout = k.collateral > 0.0 && amount >= k.collateral;
    if buyout {
        amount = k.collateral;
    }
    debit(db, outbox, me, amount).await?;
    if let Some((bidder, best)) = top {
        credit(db, outbox, bidder, best).await?;
        tell(outbox, bidder, "OnContractOutbid", id);
    }
    contract_dao::insert_bid(db, id, me, amount, now).await.map_err(db_err)?;
    if buyout {
        settle(db, sde, outbox, &mut k, me, amount, now).await?;
        tell(outbox, k.issuer, "OnContractBuyout", id);
    }
    Ok((amount, buyout))
        })},
        |value, outbox| async move { dispatch_world(value, outbox, world) },
    ).await?;
    info!("contracts: {me} bid {amount} ISK on auction {id}");
    if buyout {
        info!("contracts: auction {id} went to {me} for {amount} ISK");
    }
    Ok(Value::Bool(true))
}

/// An auction won: the items go to the winner at the start station, the bid to the issuer.
async fn settle<X: Send>(db: &(impl Executor + ?Sized), sde: &crate::sde::Sde, outbox: &mut Outbox<X>, k: &mut Contract, winner: i64, amount: f64, now: i64) -> Result<(), CallError> {
    hand_over(db, sde, outbox, k.id, winner, k.start, FLAG_HANGAR).await?;
    credit(db, outbox, k.issuer, amount).await?;
    k.acceptor = winner;
    set_status(db, k, STATUS_FINISHED, now).await?;
    tell(outbox, k.issuer, "OnContractCompleted", k.id);
    tell(outbox, winner, "OnContractCompleted", k.id);
    Ok(())
}

pub(super) async fn settle_expired(db: &Db, sde: &Arc<crate::sde::Sde>, world: &World) -> Result<(), CallError> {
    let _trading = lock().await;
    let now = filetime_now();
    let expired = contract_dao::expired_auctions_with_bids(db, TYPE_AUCTION, STATUS_OUTSTANDING, now).await.map_err(db_err)?;
    for k in expired {
        let id = k.id;
        let settled = db.transaction(
            |db, outbox: &mut Outbox| {
                let sde = sde.clone();
                Box::pin(async move {
                let sde = &*sde;
                let mut k = contract(db, id).await?;
                if let Some((winner, amount)) = top_bid(db, id).await? {
                    settle(db, sde, outbox, &mut k, winner, amount, now).await?;
                    Ok(Some((winner, amount)))
                } else {
                    Ok(None)
                }
            })},
            |value, outbox| async move { dispatch_world(value, outbox, world) },
        ).await?;
        if let Some((winner, amount)) = settled {
            info!("contracts: auction {id} went to {winner} for {amount} ISK");
        }
    }
    Ok(())
}

#[cfg(test)] mod tests;
