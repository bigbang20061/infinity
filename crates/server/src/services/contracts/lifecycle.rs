//! Contract lifecycle mechanics for contractProxy, mirroring eve/client/script/ui/shared/neocom/contracts.
//! Uses game contracts and items through database access; corporation contracts are unsupported.

use super::*;
use crate::dao::contracts::lifecycle as contract_dao;

/// `CreateContract(ContractStruct, confirm)` -> [contractID]: one contract, or one per item
/// for a multi-contract (fitted ships).
pub(super) async fn create(c: &Client, info: &Value) -> CallResult {
    let (db, world) = (&c.db, &*c.world);
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
    let start = station(db, int("startStationID")).await?;
    let end = if kind == TYPE_COURIER { Some(station(db, int("destinationID")).await?) } else { None };
    let assignee = int("assignedToID");
    if assignee == me {
        return Err(notify_error(messages::CONTRACT_SELF_ASSIGNMENT_FORBIDDEN));
    }
    let minutes = int("minutesExpire").clamp(60, 90 * 1440);
    let num_days = if kind == TYPE_COURIER { int("numDays").clamp(1, 365) as i32 } else { 0 };
    let (title, description) = (text("title", 100), text("description", 4000));
    let items: Vec<(i64, i64)> = field(info, "itemList")
        .and_then(|v| v.items())
        .unwrap_or(&[])
        .iter()
        .filter_map(|e| match e.items()? {
            [id, qty, ..] => Some((id.as_int()?, qty.as_int().unwrap_or(1))),
            _ => None,
        })
        .collect();
    let requested: Vec<(i32, i32)> = if kind == TYPE_ITEM_EXCHANGE {
        field(info, "requestItemTypeList")
            .and_then(|v| v.items())
            .unwrap_or(&[])
            .iter()
            .filter_map(|e| match e.items()? {
                [t, qty, ..] => Some((t.as_int()? as i32, qty.as_int().unwrap_or(1).clamp(1, i32::MAX as i64) as i32)),
                _ => None,
            })
            .collect()
    } else {
        vec![]
    };
    match kind {
        TYPE_AUCTION if items.is_empty() => return Err(notify_error(messages::AUCTION_REQUIRES_ITEMS)),
        TYPE_AUCTION if collateral > 0.0 && collateral < price => return Err(notify_error(messages::BUYOUT_BELOW_START_PRICE)),
        TYPE_COURIER if items.is_empty() => return Err(notify_error(messages::COURIER_REQUIRES_ITEMS)),
        TYPE_ITEM_EXCHANGE if items.is_empty() && requested.is_empty() => return Err(notify_error(messages::ITEM_EXCHANGE_REQUIRES_OFFER_OR_REQUEST)),
        _ => {}
    }
    if items.len() > MAX_ITEMS || requested.len() > MAX_ITEMS {
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
        let item = load_items(db, ItemQuery::Id(*id)).await.map_err(db_err)?.pop().ok_or_else(|| notify_error(messages::CONTRACT_ITEM_MISSING))?;
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
            status: STATUS_OUTSTANDING, issued: now, expires: now + minutes * FILETIME_MINUTE,
        }).await.map_err(db_err)?;
        for (item_id, qty) in group {
            if let Some(item) = loaded.remove(item_id) {
                escrow_item(db, outbox, item, *qty as i32, id).await?;
            }
        }
        for (type_id, qty) in requested {
            contract_dao::insert_requested_item(db, id, *type_id, *qty).await.map_err(db_err)?;
        }
        contract_dao::update_volume(db, id).await.map_err(db_err)?;
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

/// `AcceptContract(contractID, forCorp)` -> the contract.
pub(super) async fn accept(c: &Client, id: i64) -> CallResult {
    let (db, world) = (&c.db, &*c.world);
    let me = c.session.int("charid").unwrap_or(0);
    let corp = c.session.int("corpid").unwrap_or(0);
    let _trading = lock().await;
    let now = filetime_now();
    db.transaction(
        |db, outbox: &mut Outbox| Box::pin(async move {
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
                    for w in &wanted {
                        let (type_id, qty) = (w.type_id, w.quantity);
                        let stacks = load_items(db, ItemQuery::OwnerLocationFlagTypeDescending(me, k.start, FLAG_HANGAR, type_id)).await.map_err(db_err)?;
                        let have: i64 = stacks.iter().map(|s| if s.singleton { 1 } else { s.quantity as i64 }).sum();
                        if have < qty as i64 {
                            return Err(notify_error(messages::CONTRACT_REQUIRED_ITEMS_MISSING));
                        }
                        takes.push((type_id, qty, stacks));
                    }
                    debit(db, outbox, me, k.price).await?;
                    for (type_id, qty, stacks) in takes {
                        let mut left = qty;
                        for s in stacks {
                            if left == 0 {
                                break;
                            }
                            let n = if s.singleton { 1 } else { s.quantity.min(left) };
                            take_items(db, outbox, s, n).await?;
                            left -= n;
                        }
                        deliver(db, outbox, k.issuer, k.start, type_id, qty).await?;
                    }
                    credit(db, outbox, k.issuer, k.price).await?;
                    credit(db, outbox, me, k.reward).await?;
                    hand_over(db, outbox, id, me, k.start, FLAG_HANGAR).await?;
                    k.acceptor = me;
                    k.accepted = now;
                    contract_dao::set_date_accepted(db, id, now).await.map_err(db_err)?;
                    set_status(db, &mut k, STATUS_FINISHED, now).await?;
                    tell(outbox, k.issuer, "OnContractCompleted", id);
                }
                TYPE_COURIER => {
                    debit(db, outbox, me, k.collateral).await?;
                    let wrap = contract_dao::insert_package(db, TYPE_PLASTIC_WRAP, me, k.start, FLAG_HANGAR, &messages::contract_package_name(id)).await.map_err(db_err)?;
                    show_items(db, outbox, me, k.start, &[wrap], LOCATION_JUNKYARD).await?;
                    k.acceptor = me;
                    k.accepted = now;
                    k.status = STATUS_IN_PROGRESS;
                    k.package = Some(wrap);
                    contract_dao::accept_courier(db, id, me, now, STATUS_IN_PROGRESS, wrap).await.map_err(db_err)?;
                    tell(outbox, k.issuer, "OnContractAccepted", id);
                }
                _ => return Err(notify_error(messages::AUCTION_ONLY_BIDDABLE)),
            }
            Ok(k.keyval())
        }),
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
    let me = c.session.int("charid").unwrap_or(0);
    let corp = c.session.int("corpid").unwrap_or(0);
    let _trading = lock().await;
    let now = filetime_now();
    db.transaction(
        |db, outbox: &mut Outbox| Box::pin(async move {
            let mut k = contract(db, id).await?;
            match status {
                STATUS_FINISHED => {
                    if k.kind != TYPE_COURIER || k.status != STATUS_IN_PROGRESS || k.acceptor != me {
                        return Err(notify_error(messages::COURIER_DELIVERY_REQUIRES_ACCEPTOR));
                    }
                    let wrap = match k.package {
                        Some(w) => load_items(db, ItemQuery::Id(w)).await.map_err(db_err)?.pop(),
                        None => None,
                    };
                    let Some(wrap) = wrap.filter(|w| w.owner_id == me && w.location_id == k.end) else {
                        return Err(notify_error(messages::COURIER_PACKAGE_MUST_BE_AT_DESTINATION));
                    };
                    take_items(db, outbox, wrap, 1).await?;
                    hand_over(db, outbox, id, k.issuer, k.end, FLAG_HANGAR).await?;
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
                        Some(w) => load_items(db, ItemQuery::Id(w)).await.map_err(db_err)?.pop(),
                        None => None,
                    };
                    match wrap {
                        Some(w) => {
                            let (location, flag) = (w.location_id, w.flag_id);
                            take_items(db, outbox, w, 1).await?;
                            hand_over(db, outbox, id, k.acceptor, location, flag).await?;
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
        }),
        |value, outbox| async move { dispatch_world(value, outbox, world) },
    ).await.map(|value| {
        info!("contracts: {me} set contract {id} to status {status}");
        value
    })
}

/// The issuer takes an outstanding (or expired, or rejected) contract back with its escrow.
pub(super) async fn delete_one(db: &Db, world: &World, me: i64, id: i64) -> Result<(), CallError> {
    let now = filetime_now();
    db.transaction(
        |db, outbox: &mut Outbox| Box::pin(async move {
            let mut k = contract(db, id).await?;
            if k.issuer != me || !(k.status == STATUS_OUTSTANDING || k.status == STATUS_REJECTED) {
                return Err(notify_error(messages::CONTRACT_ISSUER_REQUIRED_TO_DELETE));
            }
            if k.kind == TYPE_AUCTION && top_bid(db, id).await?.is_some() {
                return Err(notify_error(messages::BIDDED_AUCTION_CANNOT_BE_DELETED));
            }
            hand_over(db, outbox, id, me, k.start, FLAG_HANGAR).await?;
            if k.kind != TYPE_AUCTION {
                credit(db, outbox, me, k.reward).await?;
            }
            set_status(db, &mut k, STATUS_DELETED, now).await?;
            Ok(())
        }),
        |value, outbox| async move { dispatch_world(value, outbox, world) },
    ).await?;
    info!("contracts: {me} deleted contract {id}");
    Ok(())
}

/// `PlaceBid(contractID, amount, forCorp)`; a bid at the buyout wins at once.
pub(super) async fn place_bid(c: &Client, id: i64, amount: f64) -> CallResult {
    let (db, world) = (&c.db, &*c.world);
    let amount = round2(amount);
    let me = c.session.int("charid").unwrap_or(0);
    let corp = c.session.int("corpid").unwrap_or(0);
    let _trading = lock().await;
    let now = filetime_now();
    let (amount, buyout) = db.transaction(
        |db, outbox: &mut Outbox| Box::pin(async move {
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
        settle(db, outbox, &mut k, me, amount, now).await?;
        tell(outbox, k.issuer, "OnContractBuyout", id);
    }
    Ok((amount, buyout))
        }),
        |value, outbox| async move { dispatch_world(value, outbox, world) },
    ).await?;
    info!("contracts: {me} bid {amount} ISK on auction {id}");
    if buyout {
        info!("contracts: auction {id} went to {me} for {amount} ISK");
    }
    Ok(Value::Bool(true))
}

/// An auction won: the items go to the winner at the start station, the bid to the issuer.
async fn settle<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, k: &mut Contract, winner: i64, amount: f64, now: i64) -> Result<(), CallError> {
    hand_over(db, outbox, k.id, winner, k.start, FLAG_HANGAR).await?;
    credit(db, outbox, k.issuer, amount).await?;
    k.acceptor = winner;
    set_status(db, k, STATUS_FINISHED, now).await?;
    tell(outbox, k.issuer, "OnContractCompleted", k.id);
    tell(outbox, winner, "OnContractCompleted", k.id);
    Ok(())
}

pub(super) async fn settle_expired(db: &Db, world: &World) -> Result<(), CallError> {
    let _trading = lock().await;
    let now = filetime_now();
    let expired = contract_dao::expired_auctions_with_bids(db, TYPE_AUCTION, STATUS_OUTSTANDING, now).await.map_err(db_err)?;
    for k in expired {
        let id = k.id;
        let settled = db.transaction(
            |db, outbox: &mut Outbox| Box::pin(async move {
                let mut k = contract(db, id).await?;
                if let Some((winner, amount)) = top_bid(db, id).await? {
                    settle(db, outbox, &mut k, winner, amount, now).await?;
                    Ok(Some((winner, amount)))
                } else {
                    Ok(None)
                }
            }),
            |value, outbox| async move { dispatch_world(value, outbox, world) },
        ).await?;
        if let Some((winner, amount)) = settled {
            info!("contracts: auction {id} went to {winner} for {amount} ISK");
        }
    }
    Ok(())
}
