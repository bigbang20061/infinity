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
use tokio_postgres::types::ToSql;
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

struct Contract {
    id: i64,
    kind: i32,
    issuer: i64,
    issuer_corp: i64,
    assignee: i64,
    acceptor: i64,
    start: i64,
    start_system: i64,
    start_region: i64,
    end: i64,
    end_system: i64,
    end_region: i64,
    price: f64,
    reward: f64,
    collateral: f64,
    title: String,
    description: String,
    volume: f64,
    num_days: i32,
    status: i32,
    issued: i64,
    expires: i64,
    accepted: i64,
    completed: i64,
    package: Option<i64>,
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

const CONTRACT_SELECT: &str = "SELECT c.contract_id, c.type, c.issuer_id, c.issuer_corp_id, c.assignee_id, c.acceptor_id,
        c.start_station_id, COALESCE(ss.solar_system_id, 0)::bigint, COALESCE(ss.region_id, 0)::bigint,
        c.end_station_id, COALESCE(es.solar_system_id, 0)::bigint, COALESCE(es.region_id, 0)::bigint,
        c.price, c.reward, c.collateral, c.title, c.description, c.volume, c.num_days, c.status,
        c.date_issued, c.date_expired, c.date_accepted, c.date_completed, c.package_id
    FROM game.contracts c
    LEFT JOIN sde.sta_stations ss ON ss.station_id = c.start_station_id
    LEFT JOIN sde.sta_stations es ON es.station_id = c.end_station_id";

fn contract_from_row(r: &tokio_postgres::Row) -> Contract {
    Contract {
        id: r.get(0),
        kind: r.get(1),
        issuer: r.get(2),
        issuer_corp: r.get(3),
        assignee: r.get(4),
        acceptor: r.get(5),
        start: r.get(6),
        start_system: r.get(7),
        start_region: r.get(8),
        end: r.get(9),
        end_system: r.get(10),
        end_region: r.get(11),
        price: r.get(12),
        reward: r.get(13),
        collateral: r.get(14),
        title: r.get(15),
        description: r.get(16),
        volume: r.get(17),
        num_days: r.get(18),
        status: r.get(19),
        issued: r.get(20),
        expires: r.get(21),
        accepted: r.get(22),
        completed: r.get(23),
        package: r.get(24),
    }
}

/// Contracts matching `where_sql` (parameters from $1), newest first.
async fn contracts(db: &(impl Executor + ?Sized), where_sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<Vec<Contract>, CallError> {
    let rows = db.query(&format!("{CONTRACT_SELECT} WHERE {where_sql} ORDER BY c.contract_id DESC LIMIT 1000"), params).await.map_err(db_err)?;
    Ok(rows.iter().map(contract_from_row).collect())
}

async fn contract(db: &(impl Executor + ?Sized), id: i64) -> Result<Contract, CallError> {
    contracts(db, "c.contract_id = $1", &[&id]).await?.pop().ok_or_else(|| user_error(messages::MSG_CON_CONTRACT_NOT_FOUND, vec![]))
}

async fn set_status(db: &(impl Executor + ?Sized), k: &mut Contract, status: i32, completed: i64) -> Result<(), CallError> {
    k.status = status;
    k.completed = completed;
    db
        .execute("UPDATE game.contracts SET status = $2, date_completed = $3, acceptor_id = $4 WHERE contract_id = $1", &[&k.id, &status, &k.completed, &k.acceptor])
        .await
        .map_err(db_err)?;
    Ok(())
}

/// contractID -> the contract's item KeyVals.
async fn items_of(db: &Db, ids: &[i64]) -> Result<HashMap<i64, Vec<Value>>, CallError> {
    let rows = db
        .query("SELECT contract_id, item_id, type_id::bigint, quantity::bigint, in_crate FROM game.contract_items WHERE contract_id = ANY($1)", &[&ids.to_vec()])
        .await
        .map_err(db_err)?;
    let mut out: HashMap<i64, Vec<Value>> = HashMap::new();
    for r in &rows {
        let contract_id: i64 = r.get(0);
        out.entry(contract_id).or_default().push(keyval([
            ("contractID", Value::Int(contract_id)),
            ("itemID", Value::Int(r.get::<_, Option<i64>>(1).unwrap_or(0))),
            ("itemTypeID", Value::Int(r.get(2))),
            ("quantity", Value::Int(r.get(3))),
            ("inCrate", Value::Bool(r.get(4))),
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
    let rows = db
        .query("SELECT bid_id, contract_id, bidder_id, amount, date FROM game.contract_bids WHERE contract_id = ANY($1) ORDER BY amount DESC", &[&ids.to_vec()])
        .await
        .map_err(db_err)?;
    let mut out: HashMap<i64, Vec<Value>> = HashMap::new();
    for r in &rows {
        let contract_id: i64 = r.get(1);
        out.entry(contract_id).or_default().push(keyval([
            ("bidID", Value::Int(r.get(0))),
            ("contractID", Value::Int(contract_id)),
            ("bidderID", Value::Int(r.get(2))),
            ("amount", Value::Float(r.get(3))),
            ("dateBid", Value::Int(r.get(4))),
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

async fn docked_at(db: &(impl Executor + ?Sized), char_id: i64) -> Result<Option<i64>, CallError> {
    Ok(db.query_opt("SELECT station_id FROM game.characters WHERE character_id = $1", &[&char_id]).await.map_err(db_err)?.and_then(|r| r.get(0)))
}

/// Tell `owner` about items now in their hangar at `station`, if they have it open.
pub(super) async fn show_items<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, owner: i64, station: i64, ids: &[i64], from: i64) -> Result<(), CallError> {
    if docked_at(db, owner).await? != Some(station) {
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
        let id: i64 = db
            .query_one(
                "INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity) VALUES ($1, $2, $3, $4, $5) RETURNING item_id",
                &[&type_id, &owner, &escrow, &FLAG_HANGAR, &quantity],
            )
            .await
            .map_err(db_err)?
            .get(0);
        (id, quantity)
    } else {
        db.execute("UPDATE game.items SET location_id = $2 WHERE item_id = $1", &[&item.item_id, &escrow]).await.map_err(db_err)?;
        let old = item.location_id;
        let quantity = if item.singleton { 1 } else { item.quantity };
        let id = item.item_id;
        let gone = Item { location_id: escrow, ..item };
        outbox.notify_pilot(owner, "OnItemChange", vec![gone.row(), Value::Dict(vec![(Value::Int(IX_LOCATION_ID), Value::Int(old))]), Value::None]);
        (id, quantity)
    };
    db
        .execute(
            "INSERT INTO game.contract_items (contract_id, item_id, type_id, quantity, in_crate) VALUES ($1, $2, $3, $4, true)",
            &[&contract_id, &item_id, &type_id, &quantity],
        )
        .await
        .map_err(db_err)?;
    Ok(())
}

async fn escrowed_ids(db: &(impl Executor + ?Sized), contract_id: i64) -> Result<Vec<i64>, CallError> {
    let rows = db
        .query("SELECT item_id FROM game.contract_items WHERE contract_id = $1 AND in_crate AND item_id IS NOT NULL", &[&contract_id])
        .await
        .map_err(db_err)?;
    Ok(rows.iter().map(|r| r.get(0)).collect())
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
    db.execute("DELETE FROM game.items WHERE location_id = ANY($1)", &[&ids]).await.map_err(db_err)?;
    db.execute("DELETE FROM game.items WHERE item_id = ANY($1)", &[&ids]).await.map_err(db_err)?;
    Ok(())
}

async fn top_bid(db: &(impl Executor + ?Sized), contract_id: i64) -> Result<Option<(i64, f64)>, CallError> {
    Ok(db
        .query_opt("SELECT bidder_id, amount FROM game.contract_bids WHERE contract_id = $1 ORDER BY amount DESC, bid_id LIMIT 1", &[&contract_id])
        .await
        .map_err(db_err)?
        .map(|r| (r.get(0), r.get(1))))
}

fn tell<X: Send>(outbox: &mut Outbox<X>, char_id: i64, event: &str, contract_id: i64) {
    if char_id != 0 {
        outbox.notify_pilot(char_id, event, vec![Value::Int(contract_id)]);
    }
}

async fn max_outstanding(db: &(impl Executor + ?Sized), char_id: i64) -> Result<i64, CallError> {
    let rows = db
        .query(
            "SELECT type_id, level FROM game.skills WHERE character_id = $1 AND type_id = ANY($2)",
            &[&char_id, &vec![SKILL_CONTRACTING, SKILL_ADVANCED_CONTRACTING]],
        )
        .await
        .map_err(db_err)?;
    let level = |skill: i32| rows.iter().find(|r| r.get::<_, i32>(0) == skill).map(|r| r.get::<_, i32>(1).clamp(0, 5) as usize).unwrap_or(0);
    Ok(NUM_CONTRACTS_BY_SKILL[level(SKILL_CONTRACTING)] + NUM_CONTRACTS_BY_ADVSKILL[level(SKILL_ADVANCED_CONTRACTING)])
}

async fn num_outstanding(db: &(impl Executor + ?Sized), char_id: i64) -> Result<i64, CallError> {
    Ok(db
        .query_one("SELECT count(*) FROM game.contracts WHERE issuer_id = $1 AND status = $2", &[&char_id, &STATUS_OUTSTANDING])
        .await
        .map_err(db_err)?
        .get(0))
}

// ---------------------------------------------------------------- lifecycle

/// `CreateContract(ContractStruct, confirm)` -> [contractID]: one contract, or one per item
/// for a multi-contract (fitted ships).
async fn create(c: &Client, info: &Value) -> CallResult {
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
        let id: i64 = db
            .query_one(
                "INSERT INTO game.contracts (type, issuer_id, issuer_corp_id, assignee_id, start_station_id, end_station_id, price, reward,
                                             collateral, title, description, num_days, status, date_issued, date_expired)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15) RETURNING contract_id",
                &[
                    &kind,
                    &me,
                    &corp,
                    &assignee,
                    &start_id,
                    &end_id,
                    &price,
                    &reward,
                    &collateral,
                    &title,
                    &description,
                    &num_days,
                    &STATUS_OUTSTANDING,
                    &now,
                    &(now + minutes * FILETIME_MINUTE),
                ],
            )
            .await
            .map_err(db_err)?
            .get(0);
        for (item_id, qty) in group {
            if let Some(item) = loaded.remove(item_id) {
                escrow_item(db, outbox, item, *qty as i32, id).await?;
            }
        }
        for (type_id, qty) in requested {
            db
                .execute(
                    "INSERT INTO game.contract_items (contract_id, type_id, quantity, in_crate) VALUES ($1, $2, $3, false)",
                    &[&id, type_id, qty],
                )
                .await
                .map_err(db_err)?;
        }
        db
            .execute(
                "UPDATE game.contracts SET volume = (SELECT COALESCE(sum(t.volume * ci.quantity), 0) FROM game.contract_items ci
                                                     JOIN sde.inv_types t USING (type_id) WHERE ci.contract_id = $1 AND ci.in_crate)
                 WHERE contract_id = $1",
                &[&id],
            )
            .await
            .map_err(db_err)?;
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
async fn accept(c: &Client, id: i64) -> CallResult {
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
                    let wanted = db
                        .query("SELECT type_id, quantity FROM game.contract_items WHERE contract_id = $1 AND NOT in_crate", &[&id])
                        .await
                        .map_err(db_err)?;
                    let mut takes = vec![];
                    for w in &wanted {
                        let (type_id, qty): (i32, i32) = (w.get(0), w.get(1));
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
                    db.execute("UPDATE game.contracts SET date_accepted = $2 WHERE contract_id = $1", &[&id, &now]).await.map_err(db_err)?;
                    set_status(db, &mut k, STATUS_FINISHED, now).await?;
                    tell(outbox, k.issuer, "OnContractCompleted", id);
                }
                TYPE_COURIER => {
                    debit(db, outbox, me, k.collateral).await?;
                    let wrap: i64 = db
                        .query_one(
                            "INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton, custom_info)
                             VALUES ($1, $2, $3, $4, 1, true, $5) RETURNING item_id",
                            &[&TYPE_PLASTIC_WRAP, &me, &k.start, &FLAG_HANGAR, &messages::contract_package_name(id)],
                        )
                        .await
                        .map_err(db_err)?
                        .get(0);
                    show_items(db, outbox, me, k.start, &[wrap], LOCATION_JUNKYARD).await?;
                    k.acceptor = me;
                    k.accepted = now;
                    k.status = STATUS_IN_PROGRESS;
                    k.package = Some(wrap);
                    db
                        .execute(
                            "UPDATE game.contracts SET acceptor_id = $2, date_accepted = $3, status = $4, package_id = $5 WHERE contract_id = $1",
                            &[&id, &me, &now, &STATUS_IN_PROGRESS, &wrap],
                        )
                        .await
                        .map_err(db_err)?;
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
async fn complete(c: &Client, id: i64, status: i32) -> CallResult {
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
async fn delete_one(db: &Db, world: &World, me: i64, id: i64) -> Result<(), CallError> {
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
async fn place_bid(c: &Client, id: i64, amount: f64) -> CallResult {
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
    db
        .execute("INSERT INTO game.contract_bids (contract_id, bidder_id, amount, date) VALUES ($1, $2, $3, $4)", &[&id, &me, &amount, &now])
        .await
        .map_err(db_err)?;
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

/// Settle expired auctions with bids, once a minute.
pub async fn settle_auctions(db: Db, world: Arc<World>) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(AUCTION_MAINTENANCE_INTERVAL_SECONDS)).await;
        if let Err(CallError::Internal(e)) = settle_expired(&db, &world).await {
            warn!("settling auctions failed: {e:#}");
        }
    }
}

async fn settle_expired(db: &Db, world: &World) -> Result<(), CallError> {
    let _trading = lock().await;
    let now = filetime_now();
    let expired = contracts(
        db,
        "c.type = $1 AND c.status = $2 AND c.date_expired <= $3 AND EXISTS (SELECT 1 FROM game.contract_bids b WHERE b.contract_id = c.contract_id)",
        &[&TYPE_AUCTION, &STATUS_OUTSTANDING, &now],
    )
    .await?;
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

// ---------------------------------------------------------------- lists

fn arg<T: ToSql + Sync + Send + 'static>(params: &mut Vec<Box<dyn ToSql + Sync + Send>>, v: T) -> String {
    params.push(Box::new(v));
    format!("${}", params.len())
}

/// `SearchContracts(**filters)` -> KeyVal(contracts, numFound, searchTime, maxResults).
async fn search(c: &Client, kw: &Value) -> CallResult {
    let db = &c.db;
    let me = c.session.int("charid").unwrap_or(0);
    let corp = c.session.int("corpid").unwrap_or(0);
    let get = |k: &str| kw.get(k).filter(|v| !v.is_none());
    let int = |k: &str| get(k).and_then(|v| v.as_int());
    let float = |k: &str| get(k).and_then(|v| v.as_float());
    let mut params: Vec<Box<dyn ToSql + Sync + Send>> = vec![];
    let mut sql = format!("c.status = {STATUS_OUTSTANDING} AND c.date_expired > {}", arg(&mut params, filetime_now()));
    let assignee = match int("availability").unwrap_or(AVAIL_PUBLIC) {
        AVAIL_MYSELF => me,
        AVAIL_MY_CORP => corp,
        AVAIL_PUBLIC => 0,
        _ => -1,
    };
    sql += &format!(" AND c.assignee_id = {}", arg(&mut params, assignee));
    match int("contractType").unwrap_or(CONTYPE_ALL) {
        CONTYPE_ALL => {}
        CONTYPE_AUCTION_AND_ITEM_EXCHANGE => sql += &format!(" AND c.type IN ({TYPE_ITEM_EXCHANGE}, {TYPE_AUCTION})"),
        t => sql += &format!(" AND c.type = {}", arg(&mut params, t as i32)),
    }
    for (key, alias) in [("locationID", "ss"), ("endLocationID", "es")] {
        if let Some(loc) = int(key) {
            let p = arg(&mut params, loc);
            let station = if alias == "ss" { "c.start_station_id" } else { "c.end_station_id" };
            sql += &format!(" AND ({station} = {p} OR {alias}.solar_system_id = {p} OR {alias}.constellation_id = {p} OR {alias}.region_id = {p})");
        }
    }
    if let Some(issuer) = int("issuerID") {
        sql += &format!(" AND c.issuer_id = {}", arg(&mut params, issuer));
    }
    for (key, column, op) in [
        ("minPrice", "price", ">="),
        ("maxPrice", "price", "<="),
        ("minReward", "reward", ">="),
        ("maxReward", "reward", "<="),
        ("minCollateral", "collateral", ">="),
        ("maxCollateral", "collateral", "<="),
        ("minVolume", "volume", ">="),
        ("maxVolume", "volume", "<="),
    ] {
        if let Some(v) = float(key) {
            sql += &format!(" AND c.{column} {op} {}", arg(&mut params, v));
        }
    }
    let crate_has = |cond: &str| format!(" AND EXISTS (SELECT 1 FROM game.contract_items ci JOIN sde.inv_types t USING (type_id) WHERE ci.contract_id = c.contract_id AND ci.in_crate AND {cond})");
    if let Some(types) = get("itemTypes").map(id_list) {
        sql += &crate_has(&format!("ci.type_id = ANY({})", arg(&mut params, types)));
    } else if let Some(group) = int("itemGroupID") {
        sql += &crate_has(&format!("t.group_id = {}", arg(&mut params, group as i32)));
    } else if let Some(category) = int("itemCategoryID") {
        sql += &crate_has(&format!("t.category_id = {}", arg(&mut params, category as i32)));
    }
    if let Some(text) = get("description").and_then(|v| v.as_text()).filter(|t| !t.trim().is_empty()) {
        sql += &format!(" AND (c.title ILIKE {p} OR c.description ILIKE {p})", p = arg(&mut params, format!("%{}%", text.trim())));
    }
    if truthy(get("excludeMultiple")) {
        sql += " AND (SELECT count(*) FROM game.contract_items ci WHERE ci.contract_id = c.contract_id AND ci.in_crate) <= 1";
    }
    if truthy(get("excludeTrade")) {
        sql += " AND NOT EXISTS (SELECT 1 FROM game.contract_items ci WHERE ci.contract_id = c.contract_id AND NOT ci.in_crate)";
    }
    if truthy(get("excludeNoBuyout")) {
        sql += &format!(" AND (c.type <> {TYPE_AUCTION} OR c.collateral > 0)");
    }
    let column = match int("sortBy").unwrap_or(0) {
        1 => "c.price",
        2 => "c.date_expired",
        3 => "c.start_station_id",
        8 => "c.reward",
        9 => "c.collateral",
        10 => "c.volume",
        _ => "c.contract_id",
    };
    let dir = if int("sortDir").unwrap_or(0) != 0 { "DESC" } else { "ASC" };
    let refs: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| p.as_ref() as &(dyn ToSql + Sync)).collect();
    let started = std::time::Instant::now();
    let rows = db
        .query(&format!("{CONTRACT_SELECT} WHERE {sql} ORDER BY {column} {dir}, c.contract_id LIMIT {MAX_SEARCH_RESULTS}"), &refs)
        .await
        .map_err(db_err)?;
    let found: Vec<Contract> = rows.iter().map(contract_from_row).collect();
    let start = (int("startNum").unwrap_or(0).max(0) as usize).min(found.len());
    let page = &found[start..(start + CONTRACTS_PER_PAGE).min(found.len())];
    Ok(keyval([
        ("contracts", Value::List(bundles(db, page).await?)),
        ("numFound", Value::Int(found.len() as i64)),
        ("searchTime", Value::Int(started.elapsed().as_millis() as i64)),
        ("maxResults", Value::Int(MAX_SEARCH_RESULTS as i64)),
    ]))
}

/// `GetContractListForOwner(ownerID, status, contractType, issuedBy, num=, startContractID=)`.
async fn list_for_owner(c: &Client, call: &Call<'_>) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let arg_int = |i: usize| call.args.get(i).and_then(|v| v.as_int());
    if arg_int(0) != Some(me) {
        return listing(&c.db, &[]).await;
    }
    let mut params: Vec<Box<dyn ToSql + Sync + Send>> = vec![];
    let p_me = arg(&mut params, me);
    let mut sql = match call.args.get(3) {
        Some(Value::Bool(false)) => format!("c.issuer_id = {p_me}"),
        Some(Value::Bool(true)) => format!("(c.acceptor_id = {p_me} OR c.assignee_id = {p_me})"),
        _ => format!("(c.issuer_id = {p_me} OR c.acceptor_id = {p_me} OR c.assignee_id = {p_me})"),
    };
    sql += &match arg_int(1).unwrap_or(STATUS_OUTSTANDING as i64) as i32 {
        STATUS_OUTSTANDING => format!(" AND c.status = {STATUS_OUTSTANDING}"),
        STATUS_IN_PROGRESS => format!(" AND c.status = {STATUS_IN_PROGRESS}"),
        STATUS_FINISHED => format!(" AND c.status NOT IN ({STATUS_OUTSTANDING}, {STATUS_IN_PROGRESS})"),
        s => format!(" AND c.status = {}", arg(&mut params, s)),
    };
    match arg_int(2) {
        None | Some(-1) | Some(CONTYPE_ALL) => {}
        Some(CONTYPE_AUCTION_AND_ITEM_EXCHANGE) => sql += &format!(" AND c.type IN ({TYPE_ITEM_EXCHANGE}, {TYPE_AUCTION})"),
        Some(t) => sql += &format!(" AND c.type = {}", arg(&mut params, t as i32)),
    }
    if let Some(before) = call.kwargs.get("startContractID").and_then(|v| v.as_int()) {
        sql += &format!(" AND c.contract_id < {}", arg(&mut params, before));
    }
    let num = call.kwargs.get("num").and_then(|v| v.as_int()).unwrap_or(100).clamp(1, 1000);
    let refs: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| p.as_ref() as &(dyn ToSql + Sync)).collect();
    let rows = c.db.query(&format!("{CONTRACT_SELECT} WHERE {sql} ORDER BY c.contract_id DESC LIMIT {num}"), &refs).await.map_err(db_err)?;
    let list: Vec<Contract> = rows.iter().map(contract_from_row).collect();
    listing(&c.db, &list).await
}

/// Contracts that need the character: their own expired or rejected ones, and courier
/// contracts overdue on either side.
async fn needing_attention(db: &Db, me: i64) -> Result<Vec<Contract>, CallError> {
    let now = filetime_now();
    contracts(
        db,
        "(c.issuer_id = $1 AND ((c.status = $2 AND c.date_expired <= $3) OR c.status = $4))
         OR ((c.issuer_id = $1 OR c.acceptor_id = $1) AND c.status = $5 AND c.date_accepted + c.num_days::bigint * $6 <= $3)",
        &[&me, &STATUS_OUTSTANDING, &now, &STATUS_REJECTED, &STATUS_IN_PROGRESS, &FILETIME_DAY],
    )
    .await
}

async fn bidding_on(db: &Db, me: i64) -> Result<Vec<Contract>, CallError> {
    contracts(
        db,
        "c.status = $2 AND EXISTS (SELECT 1 FROM game.contract_bids b WHERE b.contract_id = c.contract_id AND b.bidder_id = $1)",
        &[&me, &STATUS_OUTSTANDING],
    )
    .await
}

async fn in_progress(db: &Db, me: i64) -> Result<Vec<Contract>, CallError> {
    contracts(db, "c.acceptor_id = $1 AND c.status = $2", &[&me, &STATUS_IN_PROGRESS]).await
}

async fn assigned_to(db: &Db, me: i64, corp: i64) -> Result<Vec<Contract>, CallError> {
    contracts(db, "c.assignee_id IN ($1, $2) AND c.status = $3 AND c.date_expired > $4", &[&me, &corp, &STATUS_OUTSTANDING, &filetime_now()]).await
}

/// `CollectMyPageInfo`: the counts on the contracts start page.
async fn page_info(c: &Client) -> CallResult {
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
async fn login_info(c: &Client) -> CallResult {
    let db = &c.db;
    let me = c.session.int("charid").unwrap_or(0);
    let corp = c.session.int("corpid").unwrap_or(0);
    // contracts.DispatchAttentionNotification reads [1] (forCorp) and .contractID.
    let attention = DbRowDescriptor::new([("contractID", DbType::I8), ("forCorp", DbType::Bool)]);
    let needs: Vec<Value> = needing_attention(db, me).await?.iter().map(|k| attention.row(&[Value::Int(k.id), Value::Bool(false)])).collect();
    Ok(keyval([
        ("assignedToMe", Value::list(assigned_to(db, me, corp).await?.iter().map(Contract::keyval))),
        ("needsAttention", Value::List(needs)),
        ("inProgress", Value::list(in_progress(db, me).await?.iter().filter(|k| k.kind == TYPE_COURIER).map(Contract::keyval))),
    ]))
}

/// `GetMyContractEscrow`: ISK held by the character's contracts and top bids, and the
/// retail value of their escrowed items.
async fn escrow(c: &Client) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let r = c
        .db
        .query_one(
            "SELECT (SELECT COALESCE(sum(reward), 0) FROM game.contracts WHERE issuer_id = $1 AND type <> $2 AND status IN ($3, $4))
                  + (SELECT COALESCE(sum(b.amount), 0) FROM game.contract_bids b JOIN game.contracts k USING (contract_id)
                     WHERE b.bidder_id = $1 AND k.status = $3
                       AND b.amount = (SELECT max(amount) FROM game.contract_bids WHERE contract_id = b.contract_id)),
                    (SELECT COALESCE(sum(ci.quantity * COALESCE(r.average, 0)), 0) FROM game.contract_items ci
                     JOIN game.contracts k USING (contract_id) LEFT JOIN game.retail_prices r USING (type_id)
                     WHERE k.issuer_id = $1 AND ci.in_crate AND k.status IN ($3, $4))",
            &[&me, &TYPE_AUCTION, &STATUS_OUTSTANDING, &STATUS_IN_PROGRESS],
        )
        .await
        .map_err(db_err)?;
    Ok(keyval([("iskEscrow", Value::Float(r.get(0))), ("itemsEscrow", Value::Float(r.get(1)))]))
}

/// The character's items in a station or container, for the create-contract window.
async fn items_in(c: &Client, location: i64, hangar_only: bool) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let items = if hangar_only {
        load_items(&c.db, ItemQuery::OwnerLocationFlag(me, location, FLAG_HANGAR)).await.map_err(db_err)?
    } else {
        load_items(&c.db, ItemQuery::OwnerLocation(me, location)).await.map_err(db_err)?
    };
    Ok(Value::list(items.iter().map(Item::row)))
}

/// `SplitStack(stationID, itemID, quantity, forCorp, flag)` -> the new stack's itemID.
async fn split_stack(c: &Client, station_id: i64, item_id: i64, quantity: i64) -> CallResult {
    let (db, world) = (&c.db, &*c.world);
    let me = c.session.int("charid").unwrap_or(0);
    let _trading = lock().await;
    db.transaction(
        |db, outbox: &mut Outbox| Box::pin(async move {
            let item = load_items(db, ItemQuery::Id(item_id)).await.map_err(db_err)?.pop().ok_or_else(|| notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
            if item.owner_id != me || item.location_id != station_id || item.singleton || quantity < 1 || quantity >= item.quantity as i64 {
                return Err(notify_error(messages::STACK_CANNOT_BE_SPLIT));
            }
            let quantity = quantity as i32;
            let id = super::inventory::split_stack(db, &item, quantity, me, station_id, item.flag_id, false).await?;
            let old_quantity = item.quantity;
            let left = Item { quantity: old_quantity - quantity, ..item };
            outbox.notify_pilot(me, "OnItemChange", vec![left.row(), Value::Dict(vec![(Value::Int(IX_STACK_SIZE), Value::Int(old_quantity as i64))]), Value::None]);
            show_items(db, outbox, me, station_id, &[id], LOCATION_JUNKYARD).await?;
            Ok(Value::Int(id))
        }),
        |value, outbox| async move { dispatch_world(value, outbox, world) },
    ).await
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
                contracts(&c.db, "c.issuer_id = $1 AND c.status = $2", &[&me, &STATUS_OUTSTANDING]).await
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
            match c
                .db
                .query("SELECT location_id, count(*) FROM game.items WHERE location_id = ANY($1) AND owner_id = $2 GROUP BY 1", &[&ids, &me])
                .await
            {
                Ok(rows) => Ok(Value::Dict(rows.iter().map(|r| (Value::Int(r.get(0)), Value::Int(r.get(1)))).collect())),
                Err(e) => Err(db_err(e)),
            }
        }
        "SplitStack" => split_stack(c, int(0), int(1), int(2)).await,
        // GetCourierContractFromItemID(wrapItemID) -> the contract, or None
        "GetCourierContractFromItemID" => {
            match contracts(&c.db, "c.package_id = $1 AND c.status = $2", &[&int(0), &STATUS_IN_PROGRESS]).await {
                Ok(mut list) => Ok(list.pop().map(|k| k.keyval()).unwrap_or(Value::None)),
                Err(e) => Err(e),
            }
        }
        "DeleteNotification" | "DeleteContractNotification" => Ok(Value::None),
        _ => return None,
    })
}
