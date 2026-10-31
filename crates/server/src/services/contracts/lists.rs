//! Contract listing mechanics for contractProxy, mirroring eve/client/script/ui/shared/neocom/contracts.
//! Uses game contracts and items plus immutable SDE stations and types; corporation contracts are unsupported.
//! Search and owner listing limits use the configured contracts pagination policy.

use super::*;

// ---------------------------------------------------------------- lists


fn location_matches(location: Option<i64>, station: i64, ancestry: [Option<i64>; 3]) -> bool {
    location.is_none_or(|id| id == station || ancestry.contains(&Some(id)))
}

fn type_matches(type_id: i32, group: Option<i32>, category: Option<i32>, types: Option<&[i32]>, wanted_group: Option<i32>, wanted_category: Option<i32>) -> bool {
    if let Some(types) = types { types.contains(&type_id) }
    else if let Some(wanted) = wanted_group { group == Some(wanted) }
    else if let Some(wanted) = wanted_category { category == Some(wanted) }
    else { true }
}

/// `SearchContracts(**filters)` -> KeyVal(contracts, numFound, searchTime, maxResults).
pub(super) async fn search(c: &Client, kw: &Value) -> CallResult {
    let db = &c.db;
    let me = c.session.int("charid").unwrap_or(0);
    let corp = c.session.int("corpid").unwrap_or(0);
    let get = |k: &str| kw.get(k).filter(|v| !v.is_none());
    let int = |k: &str| get(k).and_then(|v| v.as_int());
    let float = |k: &str| get(k).and_then(|v| v.as_float());
    let assignee = match int("availability").unwrap_or(AVAIL_PUBLIC) {
        AVAIL_MYSELF => me, AVAIL_MY_CORP => corp, AVAIL_PUBLIC => 0, _ => -1,
    };
    let filters = query::Search {
        now: filetime_now(), assignee, kind: int("contractType").unwrap_or(CONTYPE_ALL),
        issuer: int("issuerID"),
        bounds: [float("minPrice"), float("maxPrice"), float("minReward"), float("maxReward"),
            float("minCollateral"), float("maxCollateral"), float("minVolume"), float("maxVolume")],
        description: get("description").and_then(|v| v.as_text()).filter(|t| !t.trim().is_empty()).map(|t| t.into_owned()),
        exclude_multiple: truthy(get("excludeMultiple")), exclude_trade: truthy(get("excludeTrade")),
        exclude_no_buyout: truthy(get("excludeNoBuyout")), sort: int("sortBy").unwrap_or(0),
        descending: int("sortDir").unwrap_or(0) != 0, outstanding: STATUS_OUTSTANDING,
        all: CONTYPE_ALL, combined: CONTYPE_AUCTION_AND_ITEM_EXCHANGE, exchange: TYPE_ITEM_EXCHANGE,
        auction: TYPE_AUCTION,
    };
    let (mut found, started) = query::search(db, filters).await.map_err(db_err)?;
    let matches_location = |id: i64, location: Option<i64>| {
        let station = c.sde.map.station(id);
        location_matches(location, id, [
            station.and_then(|s| s.solar_system_id),
            station.and_then(|s| s.constellation_id),
            station.and_then(|s| s.region_id),
        ])
    };
    found.retain(|k| matches_location(k.start, int("locationID")) && matches_location(k.end, int("endLocationID")));
    let item_types = get("itemTypes").map(id_list);
    let group = int("itemGroupID").map(|v| v as i32);
    let category = int("itemCategoryID").map(|v| v as i32);
    if item_types.is_some() || group.is_some() || category.is_some() {
        let ids: Vec<_> = found.iter().map(|k| k.id).collect();
        let matching: std::collections::HashSet<i64> = shared::items(db, &ids).await.map_err(db_err)?.into_iter()
            .filter(|item| item.in_crate && c.sde.types.get(item.type_id as i32).is_some_and(|t|
                type_matches(item.type_id as i32, t.group_id, t.category_id, item_types.as_deref(), group, category)))
            .map(|item| item.contract_id).collect();
        found.retain(|k| matching.contains(&k.id));
    }
    let policy = &crate::config::game().contracts;
    found.truncate(usize::try_from(policy.page_size_max).unwrap_or(usize::MAX));
    let start = usize::try_from(int("startNum").unwrap_or(0).max(0)).unwrap_or(usize::MAX).min(found.len());
    let page = &found[start..start.saturating_add(usize::try_from(policy.page_size).unwrap_or(usize::MAX)).min(found.len())];
    Ok(keyval([
        ("contracts", Value::List(bundles(db, &c.sde, page).await?)),
        ("numFound", Value::Int(found.len() as i64)),
        ("searchTime", Value::Int(started.elapsed().as_millis() as i64)),
        ("maxResults", Value::Int(policy.page_size_max)),
    ]))
}

/// `GetContractListForOwner(ownerID, status, contractType, issuedBy, num=, startContractID=)`.
pub(super) async fn list_for_owner(c: &Client, call: &Call<'_>) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let arg_int = |i: usize| call.args.get(i).and_then(|v| v.as_int());
    if arg_int(0) != Some(me) {
        return listing(&c.db, &c.sde, &[]).await;
    }
    let filters = query::Owner {
        me, issued_by: match call.args.get(3) { Some(Value::Bool(v)) => Some(*v), _ => None },
        status: arg_int(1).unwrap_or(STATUS_OUTSTANDING as i64) as i32, kind: arg_int(2),
        before: call.kwargs.get("startContractID").and_then(|v| v.as_int()),
        num: call.kwargs.get("num").and_then(|v| v.as_int()).unwrap_or(crate::config::game().contracts.page_size).clamp(1, crate::config::game().contracts.page_size_max),
        outstanding: STATUS_OUTSTANDING, in_progress: STATUS_IN_PROGRESS, finished: STATUS_FINISHED,
        all: CONTYPE_ALL, combined: CONTYPE_AUCTION_AND_ITEM_EXCHANGE, exchange: TYPE_ITEM_EXCHANGE, auction: TYPE_AUCTION,
    };
    let list = query::owner(&c.db, filters).await.map_err(db_err)?;
    listing(&c.db, &c.sde, &list).await
}

/// Contracts that need the character: their own expired or rejected ones, and courier
/// contracts overdue on either side.
pub(super) async fn needing_attention(db: &Db, me: i64) -> Result<Vec<Contract>, CallError> {
    query::list(db, Filter::Attention(me, filetime_now(), STATUS_OUTSTANDING, STATUS_REJECTED, STATUS_IN_PROGRESS, FILETIME_DAY)).await.map_err(db_err)
}

pub(super) async fn bidding_on(db: &Db, me: i64) -> Result<Vec<Contract>, CallError> {
    query::list(db, Filter::Bidding(me, STATUS_OUTSTANDING)).await.map_err(db_err)
}

pub(super) async fn in_progress(db: &Db, me: i64) -> Result<Vec<Contract>, CallError> {
    query::list(db, Filter::InProgress(me, STATUS_IN_PROGRESS)).await.map_err(db_err)
}

pub(super) async fn assigned_to(db: &Db, me: i64, corp: i64) -> Result<Vec<Contract>, CallError> {
    query::list(db, Filter::Assigned(me, corp, STATUS_OUTSTANDING, filetime_now())).await.map_err(db_err)
}

/// `CollectMyPageInfo`: the counts on the contracts start page.
pub(super) async fn page_info(c: &Client) -> CallResult {
    let db = &c.db;
    let me = c.session.int("charid").unwrap_or(0);
    let corp = c.session.int("corpid").unwrap_or(0);
    let open = num_outstanding(db, me).await?;
    let assigned = assigned_to(db, me, corp).await?;
    Ok(keyval([
        ("numOutstandingContracts", Value::Int(open)),
        ("numOutstandingContractsNonCorp", Value::Int(open)),
        ("numOutstandingContractsForCorp", Value::Int(0)),
        (
            "outstandingContracts",
            Value::list(assigned.iter().map(|k| Value::list([Value::Int(k.issuer), Value::Int(k.issuer_corp), Value::Int(k.assignee), Value::Int(k.kind as i64)]))),
        ),
        ("numRequiresAttention", Value::Int(needing_attention(db, me).await?.len() as i64)),
        ("numRequiresAttentionCorp", Value::Int(0)),
        ("numBiddingOn", Value::Int(bidding_on(db, me).await?.len() as i64)),
        ("numBiddingOnCorp", Value::Int(0)),
        ("numInProgress", Value::Int(in_progress(db, me).await?.len() as i64)),
        ("numInProgressCorp", Value::Int(0)),
    ]))
}

/// `GetLoginInfo`: what the neocom blinks for after login.
pub(super) async fn login_info(c: &Client) -> CallResult {
    let db = &c.db;
    let me = c.session.int("charid").unwrap_or(0);
    let corp = c.session.int("corpid").unwrap_or(0);
    // contracts.DispatchAttentionNotification reads [1] (forCorp) and .contractID.
    let attention = DbRowDescriptor::new([("contractID", DbType::I8), ("forCorp", DbType::Bool)]);
    let needs: Vec<Value> = needing_attention(db, me).await?.iter().map(|k| attention.row(&[Value::Int(k.id), Value::Bool(false)])).collect();
    Ok(keyval([
        ("assignedToMe", Value::list(assigned_to(db, me, corp).await?.iter().map(|k| k.keyval(&c.sde)))),
        ("needsAttention", Value::List(needs)),
        ("inProgress", Value::list(in_progress(db, me).await?.iter().filter(|k| k.kind == TYPE_COURIER).map(|k| k.keyval(&c.sde)))),
    ]))
}

/// `GetMyContractEscrow`: ISK held by the character's contracts and top bids, and the
/// retail value of their escrowed items.
pub(super) async fn escrow(c: &Client) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let r = crate::dao::contracts::reports::escrow(&c.db, me, TYPE_AUCTION, STATUS_OUTSTANDING, STATUS_IN_PROGRESS).await.map_err(db_err)?;
    Ok(keyval([("iskEscrow", Value::Float(r.isk)), ("itemsEscrow", Value::Float(r.items))]))
}

/// The character's items in a station or container, for the create-contract window.
pub(super) async fn items_in(c: &Client, location: i64, hangar_only: bool) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let items = if hangar_only {
        load_items(&c.db, &c.sde, ItemQuery::OwnerLocationFlag(me, location, FLAG_HANGAR)).await.map_err(db_err)?
    } else {
        load_items(&c.db, &c.sde, ItemQuery::OwnerLocation(me, location)).await.map_err(db_err)?
    };
    Ok(Value::list(items.iter().map(Item::row)))
}

/// `SplitStack(stationID, itemID, quantity, forCorp, flag)` -> the new stack's itemID.
pub(super) async fn split_stack(c: &Client, station_id: i64, item_id: i64, quantity: i64) -> CallResult {
    let (db, world) = (&c.db, &*c.world);
    let sde = &c.sde;
    let me = c.session.int("charid").unwrap_or(0);
    let _trading = lock().await;
    db.transaction(
        |db, outbox: &mut Outbox| {
            let sde = sde.clone();
            Box::pin(async move {
            let sde = &*sde;
            let item = load_items(db, sde, ItemQuery::Id(item_id)).await.map_err(db_err)?.pop().ok_or_else(|| notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
            if item.owner_id != me || item.location_id != station_id || item.singleton || quantity < 1 || quantity >= item.quantity as i64 {
                return Err(notify_error(messages::STACK_CANNOT_BE_SPLIT));
            }
            let quantity = quantity as i32;
            let id = super::super::inventory::split_stack(db, &item, quantity, me, station_id, item.flag_id, false).await?;
            let old_quantity = item.quantity;
            let left = Item { quantity: old_quantity - quantity, ..item };
            outbox.notify_pilot(me, "OnItemChange", vec![left.row(), Value::Dict(vec![(Value::Int(IX_STACK_SIZE), Value::Int(old_quantity as i64))]), Value::None]);
            show_items(db, sde, outbox, me, station_id, &[id], LOCATION_JUNKYARD).await?;
            Ok(Value::Int(id))
        })},
        |value, outbox| async move { dispatch_world(value, outbox, world) },
    ).await
}

#[cfg(test)] mod tests;

