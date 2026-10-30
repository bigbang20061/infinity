//! contractProxy: contracts between characters (eve/client/script/ui/shared/neocom/contracts).
//!
//! * Item exchange: the issuer's items (and any reward) go into escrow; the acceptor pays the
//!   price and hands over the requested items from their hangar at the start station, where
//!   both sides receive theirs.
//! * Auction: the escrowed items go to the highest bid when the contract expires, or at once
//!   to a bid that meets the buyout (the contract's collateral). Only the highest bid holds
//!   its ISK; an outbid bidder is refunded.
//! * Courier: the acceptor puts up the collateral and gets a plastic wrap at the start
//!   station; delivering the wrap to their hangar at the end station hands the goods to the
//!   issuer there and pays out reward and collateral. A failed courier contract pays the
//!   collateral to the issuer and leaves the goods with the acceptor, where the wrap is.
//!
//! Escrowed items stay in game.items, located at -contractID. An expired or rejected
//! contract waits for its issuer to delete it and take the escrow back; expired auctions
//! with bids settle on their own. Contracts cost no fees, and corporation contracts aren't
//! supported yet.
//! Contract lifecycle mutations and stack splits commit their database changes atomically;
//! their notifications are delivered only after commit.

mod lifecycle;
mod lists;

use lifecycle::{accept, complete, create, delete_one, place_bid, settle_expired};
use lists::{bidding_on, escrow, in_progress, items_in, list_for_owner, login_info, needing_attention, page_info, search, split_stack};

use crate::dao::contracts::{Contract, query::{self, Filter}, shared};
use crate::dao::items::loading::{load_items, Item, ItemQuery};
use super::items::{deliver, take_items};
use super::market::{lock, round2, station};
use super::wallets::{credit, debit};
use crate::dao::items::transfers;
use super::{db_err, keyval, notify_error, user_error, value_id_list, value_text, Call, CallError, CallResult};
use crate::client::{filetime_now, Client};
use crate::consts::{ACCOUNT_CASH, FILETIME_DAY, FILETIME_MINUTE, FLAG_HANGAR, IX_LOCATION_ID, IX_STACK_SIZE, LOCATION_JUNKYARD};
use crate::db::Db;
use crate::db::executor::Executor;
use crate::db::outbox::{dispatch_world, Outbox};
use crate::messages;
use crate::space::World;
use marshal::{DbRowDescriptor, DbType, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{info, warn};

/// Existing server policy for auction maintenance cadence.
const AUCTION_MAINTENANCE_INTERVAL_SECONDS: u64 = 60;

const TYPE_PLASTIC_WRAP: i32 = 3468;

// appConst.conType*
const TYPE_ITEM_EXCHANGE: i32 = 1;
const TYPE_AUCTION: i32 = 2;
const TYPE_COURIER: i32 = 3;
// contractscommon.CONTYPE_*
const CONTYPE_AUCTION_AND_ITEM_EXCHANGE: i64 = 10;
const CONTYPE_ALL: i64 = 11;

// appConst.conStatus*
const STATUS_OUTSTANDING: i32 = 0;
const STATUS_IN_PROGRESS: i32 = 1;
const STATUS_FINISHED: i32 = 4;
const STATUS_REJECTED: i32 = 6;
const STATUS_FAILED: i32 = 7;
const STATUS_DELETED: i32 = 8;

// appConst.conAvail*
const AVAIL_PUBLIC: i64 = 0;
const AVAIL_MYSELF: i64 = 1;
const AVAIL_MY_CORP: i64 = 2;

const SKILL_CONTRACTING: i32 = 25235;
const SKILL_ADVANCED_CONTRACTING: i32 = 73912;
// contractscommon
const NUM_CONTRACTS_BY_SKILL: [i64; 6] = [7, 16, 17, 18, 19, 20];
const NUM_CONTRACTS_BY_ADVSKILL: [i64; 6] = [21, 22, 23, 24, 25, 26];
const CONTRACTS_PER_PAGE: usize = 100;
const MAX_SEARCH_RESULTS: usize = 1000;
const MAX_ITEMS: usize = 500;

/// Attribute of a client object: an old-style instance, a new-style one (contracts.ContractStruct
/// pickles as a NewObj with its __dict__ as state), or a dict.
pub(super) fn field<'a>(v: &'a Value, name: &str) -> Option<&'a Value> {
    let found = match v {
        Value::Instance(_, state) => state.get(name),
        Value::Reduce { data, dict, .. } | Value::NewObj { data, dict, .. } => data
            .items()
            .and_then(|parts| parts.iter().find_map(|p| p.get(name)))
            .or_else(|| dict.iter().find(|(k, _)| k.as_text().is_some_and(|t| t == name)).map(|(_, v)| v)),
        _ => v.get(name),
    };
    found.filter(|v| !v.is_none())
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(v) => v.as_int().is_some_and(|i| i != 0),
        None => false,
    }
}

/// typeIDs from a list, tuple, set or the keys of a dict.
fn id_list(v: &Value) -> Vec<i32> {
    match v {
        Value::Dict(pairs) => pairs.iter().filter_map(|(k, _)| k.as_int()).map(|i| i as i32).collect(),
        _ => v.items().unwrap_or(&[]).iter().filter_map(|i| i.as_int()).map(|i| i as i32).collect(),
    }
}


impl Contract {
    fn expired(&self, now: i64) -> bool {
        self.expires <= now
    }

    fn overdue(&self, now: i64) -> bool {
        self.status == STATUS_IN_PROGRESS && self.accepted + self.num_days as i64 * FILETIME_DAY <= now
    }

    /// May `char_id` (of `corp_id`) take this contract?
    fn available_to(&self, char_id: i64, corp_id: i64) -> bool {
        self.assignee == 0 || self.assignee == char_id || self.assignee == corp_id
    }

    fn keyval(&self) -> Value {
        let int = Value::Int;
        keyval([
            ("contractID", int(self.id)),
            ("type", int(self.kind as i64)),
            ("issuerID", int(self.issuer)),
            ("issuerCorpID", int(self.issuer_corp)),
            ("issuerAllianceID", Value::None),
            ("issuerWalletKey", int(ACCOUNT_CASH as i64)),
            ("forCorp", Value::Bool(false)),
            ("availability", int(if self.assignee == 0 { AVAIL_PUBLIC } else { AVAIL_MYSELF })),
            ("assigneeID", int(self.assignee)),
            ("acceptorID", int(self.acceptor)),
            ("acceptorWalletKey", int(ACCOUNT_CASH as i64)),
            ("dateIssued", int(self.issued)),
            ("dateExpired", int(self.expires)),
            ("dateAccepted", int(self.accepted)),
            ("dateCompleted", int(self.completed)),
            ("numDays", int(self.num_days as i64)),
            ("expires", int(self.accepted + self.num_days as i64 * FILETIME_DAY)),
            ("startStationID", int(self.start)),
            ("startSolarSystemID", int(self.start_system)),
            ("startRegionID", int(self.start_region)),
            ("endStationID", int(self.end)),
            ("endSolarSystemID", int(self.end_system)),
            ("endRegionID", int(self.end_region)),
            ("price", Value::Float(self.price)),
            ("reward", Value::Float(self.reward)),
            ("collateral", Value::Float(self.collateral)),
            ("title", Value::unicode(self.title.as_str())),
            ("description", Value::unicode(self.description.as_str())),
            ("status", int(self.status as i64)),
            ("volume", Value::Float(self.volume)),
            ("crateID", int(0)),
        ])
    }
}


async fn contract(db: &(impl Executor + ?Sized), id: i64) -> Result<Contract, CallError> {
    query::list(db, Filter::Id(id)).await.map_err(db_err)?.pop().ok_or_else(|| user_error(messages::MSG_CON_CONTRACT_NOT_FOUND, vec![]))
}

async fn set_status(db: &(impl Executor + ?Sized), k: &mut Contract, status: i32, completed: i64) -> Result<(), CallError> {
    k.status = status;
    k.completed = completed;
    shared::set_status(db, k.id, status, k.completed, k.acceptor).await.map_err(db_err)?;
    Ok(())
}

/// contractID -> the contract's item KeyVals.
async fn items_of(db: &Db, ids: &[i64]) -> Result<HashMap<i64, Vec<Value>>, CallError> {
    let rows = shared::items(db, ids).await.map_err(db_err)?;
    let mut out: HashMap<i64, Vec<Value>> = HashMap::new();
    for r in &rows {
        let contract_id = r.contract_id;
        out.entry(contract_id).or_default().push(keyval([
            ("contractID", Value::Int(contract_id)),
            ("itemID", Value::Int(r.item_id.unwrap_or(0))),
            ("itemTypeID", Value::Int(r.type_id)),
            ("quantity", Value::Int(r.quantity)),
            ("inCrate", Value::Bool(r.in_crate)),
            ("parentID", Value::Int(0)),
            ("productivityLevel", Value::None),
            ("materialLevel", Value::None),
            ("copy", Value::Int(0)),
            ("licensedProductionRunsRemaining", Value::None),
            ("damage", Value::Float(0.0)),
            ("flagID", Value::Int(FLAG_HANGAR as i64)),
        ]));
    }
    Ok(out)
}

/// contractID -> bid KeyVals, highest first.
async fn bids_of(db: &Db, ids: &[i64]) -> Result<HashMap<i64, Vec<Value>>, CallError> {
    let rows = shared::bids(db, ids).await.map_err(db_err)?;
    let mut out: HashMap<i64, Vec<Value>> = HashMap::new();
    for r in &rows {
        let contract_id = r.contract_id;
        out.entry(contract_id).or_default().push(keyval([
            ("bidID", Value::Int(r.bid_id)),
            ("contractID", Value::Int(contract_id)),
            ("bidderID", Value::Int(r.bidder_id)),
            ("amount", Value::Float(r.amount)),
            ("dateBid", Value::Int(r.date)),
        ]));
    }
    Ok(out)
}

/// `KeyVal(contract, items, bids)` per contract (GetContract, SearchContracts).
async fn bundles(db: &Db, list: &[Contract]) -> Result<Vec<Value>, CallError> {
    let ids: Vec<i64> = list.iter().map(|k| k.id).collect();
    let (mut items, mut bids) = (items_of(db, &ids).await?, bids_of(db, &ids).await?);
    Ok(list
        .iter()
        .map(|k| {
            keyval([
                ("contract", k.keyval()),
                ("items", Value::List(items.remove(&k.id).unwrap_or_default())),
                ("bids", Value::List(bids.remove(&k.id).unwrap_or_default())),
            ])
        })
        .collect())
}

/// `KeyVal(contracts, items={contractID: items})` (the my-contracts lists).
async fn listing(db: &Db, list: &[Contract]) -> CallResult {
    let ids: Vec<i64> = list.iter().map(|k| k.id).collect();
    let items = items_of(db, &ids).await?;
    Ok(keyval([
        ("contracts", Value::list(list.iter().map(Contract::keyval))),
        ("items", Value::Dict(items.into_iter().map(|(id, v)| (Value::Int(id), Value::List(v))).collect())),
    ]))
}


/// Tell `owner` about items now in their hangar at `station`, if they have it open.
pub(super) async fn show_items<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, owner: i64, station: i64, ids: &[i64], from: i64) -> Result<(), CallError> {
    if transfers::docked(db, owner).await.map_err(db_err)? != Some(station) {
        return Ok(());
    }
    for item in load_items(db, ItemQuery::Ids(ids)).await.map_err(db_err)? {
        outbox.notify_pilot(owner, "OnItemChange", vec![item.row(), Value::Dict(vec![(Value::Int(IX_LOCATION_ID), Value::Int(from))]), Value::None]);
    }
    Ok(())
}

/// Move `quantity` of an item from its owner's hangar into the contract's escrow.
async fn escrow_item<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, item: Item, quantity: i32, contract_id: i64) -> Result<(), CallError> {
    let (type_id, owner) = (item.type_id, item.owner_id);
    let escrow = -contract_id;
    let (item_id, quantity) = if !item.singleton && quantity < item.quantity {
        take_items(db, outbox, item, quantity).await?;
        let id = transfers::insert(db, type_id, owner, escrow, FLAG_HANGAR, quantity).await.map_err(db_err)?;
        (id, quantity)
    } else {
        shared::move_item(db, item.item_id, escrow).await.map_err(db_err)?;
        let old = item.location_id;
        let quantity = if item.singleton { 1 } else { item.quantity };
        let id = item.item_id;
        let gone = Item { location_id: escrow, ..item };
        outbox.notify_pilot(owner, "OnItemChange", vec![gone.row(), Value::Dict(vec![(Value::Int(IX_LOCATION_ID), Value::Int(old))]), Value::None]);
        (id, quantity)
    };
    shared::insert_item(db, contract_id, item_id, type_id, quantity).await.map_err(db_err)?;
    Ok(())
}

async fn escrowed_ids(db: &(impl Executor + ?Sized), contract_id: i64) -> Result<Vec<i64>, CallError> {
    shared::escrowed_ids(db, contract_id).await.map_err(db_err)
}

/// Give the escrowed items (and whatever they hold) to `owner` at `location`/`flag`.
async fn hand_over<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, contract_id: i64, owner: i64, location: i64, flag: i32) -> Result<(), CallError> {
    let ids = escrowed_ids(db, contract_id).await?;
    transfers::hand_over(db, &ids, owner, location, flag).await.map_err(db_err)?;
    show_items(db, outbox, owner, location, &ids, -contract_id).await
}

/// Lose the escrowed items (a courier package that no longer exists).
async fn destroy_escrow(db: &(impl Executor + ?Sized), contract_id: i64) -> Result<(), CallError> {
    let ids = escrowed_ids(db, contract_id).await?;
    shared::destroy_escrow(db, &ids).await.map_err(db_err)?;
    Ok(())
}

async fn top_bid(db: &(impl Executor + ?Sized), contract_id: i64) -> Result<Option<(i64, f64)>, CallError> {
    Ok(shared::top_bid(db, contract_id).await.map_err(db_err)?.map(|r| (r.bidder_id, r.amount)))
}

fn tell<X: Send>(outbox: &mut Outbox<X>, char_id: i64, event: &str, contract_id: i64) {
    if char_id != 0 {
        outbox.notify_pilot(char_id, event, vec![Value::Int(contract_id)]);
    }
}

async fn max_outstanding(db: &(impl Executor + ?Sized), char_id: i64) -> Result<i64, CallError> {
    let rows = crate::dao::skills::market::levels(db, char_id, &[SKILL_CONTRACTING, SKILL_ADVANCED_CONTRACTING]).await.map_err(db_err)?;
    let level = |skill: i32| rows.iter().find(|r| r.type_id == skill).map(|r| r.level.clamp(0, 5) as usize).unwrap_or(0);
    Ok(NUM_CONTRACTS_BY_SKILL[level(SKILL_CONTRACTING)] + NUM_CONTRACTS_BY_ADVSKILL[level(SKILL_ADVANCED_CONTRACTING)])
}

async fn num_outstanding(db: &(impl Executor + ?Sized), char_id: i64) -> Result<i64, CallError> {
    shared::num_outstanding(db, char_id, STATUS_OUTSTANDING).await.map_err(db_err)
}

// ---------------------------------------------------------------- lifecycle


/// Settle expired auctions with bids, once a minute.
pub async fn settle_auctions(db: Db, world: Arc<World>) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(AUCTION_MAINTENANCE_INTERVAL_SECONDS)).await;
        if let Err(CallError::Internal(e)) = settle_expired(&db, &world).await {
            warn!("settling auctions failed: {e:#}");
        }
    }
}


pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service != "contractProxy" {
        return None;
    }
    let int = |i: usize| call.args.get(i).and_then(|v| v.as_int()).unwrap_or(0);
    let me = c.session.int("charid").unwrap_or(0);
    let for_corp = |i: usize| truthy(call.args.get(i));
    Some(match call.method {
        "CreateContract" => match call.args.first() {
            Some(info) => create(c, info).await,
            None => Err(notify_error(messages::CONTRACT_CREATION_REQUEST_MISSING)),
        },
        "GetContract" => match contract(&c.db, int(0)).await {
            Ok(k) => bundles(&c.db, &[k]).await.map(|mut b| b.pop().unwrap_or(Value::None)),
            Err(e) => Err(e),
        },
        "AcceptContract" if for_corp(1) => Err(notify_error(messages::CORPORATION_CONTRACTS_UNSUPPORTED)),
        "AcceptContract" => accept(c, int(0)).await,
        "CompleteContract" => complete(c, int(0), int(1) as i32).await,
        "DeleteContract" => {
            let _trading = lock().await;
            delete_one(&c.db, &c.world, me, int(0)).await.map(|_| Value::Bool(true))
        }
        // DeleteMultipleContracts([contractID]) -> (deleted, failed)
        "DeleteMultipleContracts" => {
            let ids = value_id_list(call.args.first());
            let _trading = lock().await;
            let (mut deleted, mut failed) = (vec![], vec![]);
            for id in ids {
                match delete_one(&c.db, &c.world, me, id).await {
                    Ok(()) => deleted.push(Value::Int(id)),
                    Err(CallError::User { .. }) => failed.push(Value::Int(id)),
                    Err(e) => return Some(Err(e)),
                }
            }
            Ok(Value::tuple([Value::List(deleted), Value::List(failed)]))
        }
        "PlaceBid" if for_corp(2) => Err(notify_error(messages::CORPORATION_CONTRACTS_UNSUPPORTED)),
        "PlaceBid" => place_bid(c, int(0), call.args.get(1).and_then(|v| v.as_float()).unwrap_or(0.0)).await,
        // FinishAuction(contractID, isIssuer): expired auctions settle on their own; settle now if due.
        "FinishAuction" => {
            let r = settle_expired(&c.db, &c.world).await;
            r.map(|_| Value::Bool(true))
        }
        "SearchContracts" => search(c, call.kwargs).await,
        "GetContractListForOwner" => list_for_owner(c, call).await,
        "GetMyBids" if for_corp(0) => listing(&c.db, &[]).await,
        "GetMyBids" => match bidding_on(&c.db, me).await {
            Ok(list) => listing(&c.db, &list).await,
            Err(e) => Err(e),
        },
        "GetMyExpiredContractList" if for_corp(0) => listing(&c.db, &[]).await,
        "GetMyExpiredContractList" => match needing_attention(&c.db, me).await {
            Ok(list) => listing(&c.db, &list).await,
            Err(e) => Err(e),
        },
        // GetMyCurrentContractList(isAccepted, forCorp)
        "GetMyCurrentContractList" if for_corp(1) => listing(&c.db, &[]).await,
        "GetMyCurrentContractList" => {
            let list = if for_corp(0) {
                in_progress(&c.db, me).await
            } else {
                query::list(&c.db, Filter::Issuer(me, STATUS_OUTSTANDING)).await.map_err(db_err)
            };
            match list {
                Ok(list) => listing(&c.db, &list).await,
                Err(e) => Err(e),
            }
        }
        "CollectMyPageInfo" => page_info(c).await,
        "NumOutstandingContracts" => num_outstanding(&c.db, me).await.map(Value::Int),
        "GetLoginInfo" => login_info(c).await,
        "GetMyContractEscrow" => escrow(c).await,
        // GetItemsInDockableLocation(stationID, forCorp)
        "GetItemsInDockableLocation" => items_in(c, int(0), true).await,
        // GetItemsInContainer(stationID, containerID, forCorp, flag)
        "GetItemsInContainer" => items_in(c, int(1), false).await,
        // GetNumItemsInContainers(stationID, [containerID], forCorp, flag) -> {containerID: count}
        "GetNumItemsInContainers" => {
            let ids = value_id_list(call.args.get(1));
            match crate::dao::contracts::reports::container_counts(&c.db, &ids, me).await {
                Ok(rows) => Ok(Value::Dict(rows.iter().map(|r| (Value::Int(r.location_id), Value::Int(r.count))).collect())),
                Err(e) => Err(db_err(e)),
            }
        }
        "SplitStack" => split_stack(c, int(0), int(1), int(2)).await,
        // GetCourierContractFromItemID(wrapItemID) -> the contract, or None
        "GetCourierContractFromItemID" => {
            match query::list(&c.db, Filter::Package(int(0), STATUS_IN_PROGRESS)).await.map_err(db_err) {
                Ok(mut list) => Ok(list.pop().map(|k| k.keyval()).unwrap_or(Value::None)),
                Err(e) => Err(e),
            }
        }
        "DeleteNotification" | "DeleteContractNotification" => Ok(Value::None),
        _ => return None,
    })
}
