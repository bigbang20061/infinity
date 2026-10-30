//! marketProxy: regional order books (eve/client/script/ui/services/marketsvc.py).
//!
//! The book holds the NPC orders seeded at the starter stations (sql/market_seed.sql, and
//! region-wide buy orders at consts::NPC_BID_RATIO of those prices) and players' orders. NPC
//! stock and demand are unlimited, so fills never consume NPC orders.
//!
//! A trade happens at the resting order's price and delivers to the buyer's hangar in the
//! seller's station. The seller pays sales tax and an order that rests pays the broker's
//! fee, both as marketutil computes them (skilllimits, brokerFee) at zero standings. A sell
//! order holds its items and a buy order its ISK in escrow until it fills, is cancelled or
//! expires; closed orders move to game.market_order_history, and every fill is a row of
//! game.market_transactions (wallet transactions and price history). Ranges in jumps are
//! counted on the stargate graph (sde.map_jumps) within the region.

use super::dogma::{load_items_db, Item};
use super::{crowset, db_err, keyval, notify_error, user_error, value_field, Call, CallError, CallResult};
use crate::client::{filetime_now, Client};
use crate::consts::{ACCOUNT_CASH, FILETIME_DAY, FLAG_HANGAR, IX_LOCATION_ID, IX_STACK_SIZE, LOCATION_JUNKYARD};
use crate::db::Db;
use crate::space::World;
use marshal::{DbRowDescriptor, DbType, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};
use tracing::{info, warn};

/// Existing server policy for market order maintenance cadence.
const ORDER_MAINTENANCE_INTERVAL_SECONDS: u64 = 60;

/// Existing server policy for closed market order history, in days.
const ORDER_HISTORY_DAYS: i64 = 90;
/// Existing server policy for the default transaction history, in days.
const DEFAULT_TRANSACTION_HISTORY_DAYS: i64 = 30;
/// Existing server policy for market price history, in days.
const PRICE_HISTORY_DAYS: i64 = 365;

const RANGE_STATION: i64 = -1;
const RANGE_SOLAR_SYSTEM: i64 = 0;
const RANGE_REGION: i64 = 32767;
/// marketutil.const.MAX_ORDER_PRICE
const MAX_ORDER_PRICE: f64 = 9_223_372_036_854.0;
/// Longest order the sell and buy windows offer (days).
const MAX_DURATION: i64 = 90;
/// What the client shows for a system it can't route to.
const UNREACHABLE: i64 = 1_000_000;

// marketutil.const, appConst.mktMinimumFee and MKT_TRANSACTION_TAX_DEFAULT
const BROKER_FEE: f64 = 0.03;
const BROKER_RELATIONS_PER_LEVEL: f64 = 0.003;
const MIN_BROKER_FEE: f64 = 100.0;
const SALES_TAX: f64 = 0.08;
const ACCOUNTING_PER_LEVEL: f64 = 0.11;

// marketHistory orderState
const STATE_CLOSED: i32 = 2; // filled or expired
const STATE_CANCELLED: i32 = 3;

const SKILL_TRADE: i32 = 3443;
const SKILL_RETAIL: i32 = 3444;
const SKILL_BROKER_RELATIONS: i32 = 3446;
const SKILL_WHOLESALE: i32 = 16596;
const SKILL_MARGIN_TRADING: i32 = 16597;
const SKILL_ACCOUNTING: i32 = 16622;
const SKILL_TYCOON: i32 = 18580;

pub(super) fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

fn valid_price(price: f64) -> Result<f64, CallError> {
    if price.is_finite() && (0.01..=MAX_ORDER_PRICE).contains(&price) {
        Ok(round2(price))
    } else {
        Err(notify_error("价格无效。"))
    }
}

/// Columns of an order row; the client sets `jumps` itself (RefreshJumps).
fn order_header() -> DbRowDescriptor {
    DbRowDescriptor::new([
        ("price", DbType::R8),
        ("volRemaining", DbType::I4),
        ("typeID", DbType::I4),
        ("range", DbType::I2),
        ("orderID", DbType::I8),
        ("volEntered", DbType::I4),
        ("minVolume", DbType::I4),
        ("bid", DbType::Bool),
        ("issueDate", DbType::FileTime),
        ("duration", DbType::I2),
        ("stationID", DbType::I8),
        ("regionID", DbType::I4),
        ("solarSystemID", DbType::I4),
        ("constellationID", DbType::I4),
        ("jumps", DbType::I2),
    ])
}

const ORDER_COLUMNS: &str = "price, vol_remaining::bigint, type_id::bigint, range::bigint, order_id, vol_entered::bigint,
    min_volume::bigint, bid, COALESCE(issued, $1), duration::bigint, station_id, region_id, solar_system_id, constellation_id";

fn today() -> i64 {
    filetime_now() / FILETIME_DAY * FILETIME_DAY
}

/// `GetOrders(typeID)` -> (asks, bids) in the session's region.
async fn orders(c: &Client, type_id: i64) -> CallResult {
    let region = c.session.int("regionid").unwrap_or(0);
    let rows = c
        .db
        .client
        .query(
            &format!("SELECT {ORDER_COLUMNS} FROM game.market_orders WHERE type_id = $2 AND region_id = $3 AND vol_remaining > 0 ORDER BY price"),
            &[&today(), &(type_id as i32), &region],
        )
        .await
        .map_err(db_err)?;
    let header = order_header();
    let (mut asks, mut bids) = (vec![], vec![]);
    for r in &rows {
        let mut line = vec![Value::Float(r.get(0))];
        line.extend((1..7).map(|i| Value::Int(r.get(i))));
        line.push(Value::Bool(r.get(7)));
        line.extend((8..14).map(|i| Value::Int(r.get(i))));
        line.push(Value::Int(0));
        let row = header.row(&line);
        if r.get(7) {
            bids.push(row)
        } else {
            asks.push(row)
        }
    }
    Ok(Value::tuple([crowset(&header, asks), crowset(&header, bids)]))
}

/// Cheapest ask per type within `column = id`: {typeID: best}.
async fn best_asks(c: &Client, column: &str, id: i64, as_rows: bool) -> CallResult {
    let rows = c
        .db
        .client
        .query(
            &format!(
                "SELECT DISTINCT ON (type_id) type_id::bigint, price, vol_remaining::bigint, station_id
                 FROM game.market_orders WHERE {column} = $1 AND NOT bid AND vol_remaining > 0 ORDER BY type_id, price"
            ),
            &[&id],
        )
        .await
        .map_err(db_err)?;
    // Station/system answers are tuples (marketutil.ConvertTuplesToBestByOrders); the
    // region answer is used as-is, by attribute.
    let header = DbRowDescriptor::new([("price", DbType::R8), ("volRemaining", DbType::I4), ("typeID", DbType::I4), ("stationID", DbType::I8)]);
    Ok(Value::Dict(
        rows.iter()
            .map(|r| {
                let line = [Value::Float(r.get(1)), Value::Int(r.get(2)), Value::Int(r.get(0)), Value::Int(r.get(3))];
                (Value::Int(r.get(0)), if as_rows { header.row(&line) } else { Value::tuple(line) })
            })
            .collect(),
    ))
}

// ---------------------------------------------------------------- trading

/// Trades run one at a time, so each sees the book as the last one left it.
pub(super) async fn lock() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(Default::default).lock().await
}

#[derive(Clone, Copy)]
pub(super) struct Station {
    pub(super) id: i64,
    pub(super) system: i64,
    pub(super) constellation: i64,
    pub(super) region: i64,
    pub(super) owner: i64,
}

pub(super) async fn station(db: &Db, id: i64) -> Result<Station, CallError> {
    let r = db
        .client
        .query_opt(
            "SELECT station_id::bigint, solar_system_id::bigint, constellation_id::bigint, region_id::bigint, COALESCE(owner_id, 0)::bigint
             FROM sde.sta_stations WHERE station_id = $1::bigint",
            &[&id],
        )
        .await
        .map_err(db_err)?
        .ok_or_else(|| notify_error("该地点没有市场。"))?;
    Ok(Station { id: r.get(0), system: r.get(1), constellation: r.get(2), region: r.get(3), owner: r.get(4) })
}

/// Solar system -> the systems one stargate jump away.
pub(super) async fn graph(db: &Db) -> Result<&'static HashMap<i64, Vec<i64>>, CallError> {
    static GRAPH: OnceLock<HashMap<i64, Vec<i64>>> = OnceLock::new();
    if let Some(g) = GRAPH.get() {
        return Ok(g);
    }
    let rows = db.client.query("SELECT from_solar_system_id::bigint, to_solar_system_id::bigint FROM sde.map_jumps", &[]).await.map_err(db_err)?;
    let mut g: HashMap<i64, Vec<i64>> = HashMap::new();
    for r in &rows {
        g.entry(r.get(0)).or_default().push(r.get(1));
    }
    Ok(GRAPH.get_or_init(|| g))
}

/// Stargate jumps from `from` to `to`, looking no further than `max`.
pub(super) fn jumps(g: &HashMap<i64, Vec<i64>>, from: i64, to: i64, max: i64) -> Option<i64> {
    if from == to {
        return Some(0);
    }
    let mut seen = HashSet::from([from]);
    let mut frontier = vec![from];
    for d in 1..=max {
        let mut next = vec![];
        for s in frontier {
            for &n in g.get(&s).into_iter().flatten() {
                if n == to {
                    return Some(d);
                }
                if seen.insert(n) {
                    next.push(n);
                }
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    None
}

/// May an order of `range` placed at `at` trade with something at `other`?
fn in_range(g: &HashMap<i64, Vec<i64>>, range: i64, at: &Station, other: &Station) -> bool {
    match range {
        RANGE_STATION => at.id == other.id,
        RANGE_SOLAR_SYSTEM => at.system == other.system,
        r if r >= RANGE_REGION => at.region == other.region,
        r => at.region == other.region && jumps(g, at.system, other.system, r).is_some(),
    }
}

/// What a character's trade skills make of fees, taxes and order slots (marketutil.skilllimits).
struct Skills {
    order_cap: i64,
    sales_tax: f64,
    broker_fee: f64,
    modification_discount: f64,
}

async fn skills(db: &Db, char_id: i64) -> Result<Skills, CallError> {
    let wanted = vec![SKILL_TRADE, SKILL_RETAIL, SKILL_BROKER_RELATIONS, SKILL_WHOLESALE, SKILL_MARGIN_TRADING, SKILL_ACCOUNTING, SKILL_TYCOON];
    let rows = db
        .client
        .query("SELECT type_id, level FROM game.skills WHERE character_id = $1 AND type_id = ANY($2)", &[&char_id, &wanted])
        .await
        .map_err(db_err)?;
    let levels: HashMap<i32, i32> = rows.iter().map(|r| (r.get(0), r.get(1))).collect();
    let l = |skill: i32| levels.get(&skill).copied().unwrap_or(0) as f64;
    Ok(Skills {
        order_cap: (5.0 + 4.0 * l(SKILL_TRADE) + 8.0 * l(SKILL_RETAIL) + 16.0 * l(SKILL_WHOLESALE) + 32.0 * l(SKILL_TYCOON)) as i64,
        sales_tax: SALES_TAX * (1.0 - ACCOUNTING_PER_LEVEL * l(SKILL_ACCOUNTING)),
        broker_fee: BROKER_FEE - BROKER_RELATIONS_PER_LEVEL * l(SKILL_BROKER_RELATIONS),
        modification_discount: 0.5 + 0.06 * l(SKILL_MARGIN_TRADING),
    })
}

fn broker_fee(rate: f64, amount: f64) -> f64 {
    round2((rate * amount).max(MIN_BROKER_FEE))
}

async fn check_order_slots(db: &Db, char_id: i64, skills: &Skills) -> Result<(), CallError> {
    let open: i64 = db.client.query_one("SELECT count(*) FROM game.market_orders WHERE char_id = $1", &[&char_id]).await.map_err(db_err)?.get(0);
    if open >= skills.order_cap {
        return Err(notify_error(&format!("你已有 {open} 个未完成的市场订单，达到了交易技能允许的上限。")));
    }
    Ok(())
}

async fn balance(db: &Db, char_id: i64) -> Result<f64, CallError> {
    Ok(db.client.query_one("SELECT balance::float8 FROM game.characters WHERE character_id = $1", &[&char_id]).await.map_err(db_err)?.get(0))
}

fn not_enough_money(have: f64, amount: f64) -> CallError {
    user_error("NotEnoughMoney", vec![("balance".into(), Value::Float(have)), ("amount".into(), Value::Float(amount))])
}

async fn require_balance(db: &Db, char_id: i64, amount: f64) -> Result<(), CallError> {
    let have = balance(db, char_id).await?;
    if have < amount {
        return Err(not_enough_money(have, amount));
    }
    Ok(())
}

pub(super) async fn credit(db: &Db, world: &World, char_id: i64, amount: f64) -> Result<(), CallError> {
    if amount <= 0.0 {
        return Ok(());
    }
    let balance: f64 = db
        .client
        .query_one(
            "UPDATE game.characters SET balance = balance + $2::float8::numeric WHERE character_id = $1 RETURNING balance::float8",
            &[&char_id, &amount],
        )
        .await
        .map_err(db_err)?
        .get(0);
    world.notify_pilot(char_id, "OnAccountChange", vec![Value::str("cash"), Value::Int(char_id), Value::Float(balance)]);
    Ok(())
}

pub(super) async fn debit(db: &Db, world: &World, char_id: i64, amount: f64) -> Result<(), CallError> {
    if amount <= 0.0 {
        return Ok(());
    }
    let row = db
        .client
        .query_opt(
            "UPDATE game.characters SET balance = balance - $2::float8::numeric
             WHERE character_id = $1 AND balance >= $2::float8::numeric RETURNING balance::float8",
            &[&char_id, &amount],
        )
        .await
        .map_err(db_err)?;
    match row {
        Some(r) => {
            world.notify_pilot(char_id, "OnAccountChange", vec![Value::str("cash"), Value::Int(char_id), Value::Float(r.get(0))]);
            Ok(())
        }
        None => Err(not_enough_money(balance(db, char_id).await?, amount)),
    }
}

/// Take `quantity` off a stack, deleting it when all of it goes, and tell its owner.
pub(super) async fn take_items(db: &Db, world: &World, item: Item, quantity: i32) -> Result<(), CallError> {
    let owner = item.owner_id;
    if quantity >= item.quantity {
        db.client.execute("DELETE FROM game.items WHERE item_id = $1", &[&item.item_id]).await.map_err(db_err)?;
        let old = item.location_id;
        let gone = Item { location_id: LOCATION_JUNKYARD, ..item };
        world.notify_pilot(owner, "OnItemChange", vec![gone.row(), Value::Dict(vec![(Value::Int(IX_LOCATION_ID), Value::Int(old))]), Value::None]);
    } else {
        db.client.execute("UPDATE game.items SET quantity = quantity - $2 WHERE item_id = $1", &[&item.item_id, &quantity]).await.map_err(db_err)?;
        let left = Item { quantity: item.quantity - quantity, ..item };
        let change = Value::Dict(vec![(Value::Int(IX_STACK_SIZE), Value::Int((left.quantity + quantity) as i64))]);
        world.notify_pilot(owner, "OnItemChange", vec![left.row(), change, Value::None]);
    }
    Ok(())
}

/// Put `quantity` x `type_id` in `owner`'s hangar at `station`, onto a stack there if there is one.
pub(super) async fn deliver(db: &Db, world: &World, owner: i64, station: i64, type_id: i32, quantity: i32) -> Result<(), CallError> {
    let stack = db
        .client
        .query_opt(
            "SELECT item_id, quantity FROM game.items WHERE location_id = $1 AND flag_id = $2 AND type_id = $3 AND owner_id = $4 AND NOT singleton LIMIT 1",
            &[&station, &FLAG_HANGAR, &type_id, &owner],
        )
        .await
        .map_err(db_err)?;
    let (item_id, change) = match stack {
        Some(r) => {
            let (id, old): (i64, i32) = (r.get(0), r.get(1));
            db.client.execute("UPDATE game.items SET quantity = quantity + $2 WHERE item_id = $1", &[&id, &quantity]).await.map_err(db_err)?;
            (id, (IX_STACK_SIZE, old as i64))
        }
        None => {
            let id: i64 = db
                .client
                .query_one(
                    "INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity) VALUES ($1, $2, $3, $4, $5) RETURNING item_id",
                    &[&type_id, &owner, &station, &FLAG_HANGAR, &quantity],
                )
                .await
                .map_err(db_err)?
                .get(0);
            (id, (IX_LOCATION_ID, 0))
        }
    };
    // Only a pilot docked there has that hangar loaded.
    let docked: Option<i64> =
        db.client.query_opt("SELECT station_id FROM game.characters WHERE character_id = $1", &[&owner]).await.map_err(db_err)?.and_then(|r| r.get(0));
    if docked == Some(station) {
        if let Some(item) = load_items_db(db, "i.item_id = $1", &[&item_id]).await?.pop() {
            let change = Value::Dict(vec![(Value::Int(change.0), Value::Int(change.1))]);
            world.notify_pilot(owner, "OnItemChange", vec![item.row(), change, Value::None]);
        }
    }
    Ok(())
}

async fn record(db: &Db, type_id: i32, quantity: i32, price: f64, buyer: i64, seller: i64, at: &Station) -> Result<(), CallError> {
    db.client
        .execute(
            "INSERT INTO game.market_transactions (type_id, quantity, price, buyer_id, seller_id, station_id, region_id, date)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            &[&type_id, &quantity, &price, &buyer, &seller, &at.id, &at.region, &filetime_now()],
        )
        .await
        .map_err(db_err)?;
    Ok(())
}

struct Order {
    pub(super) id: i64,
    type_id: i32,
    /// None for NPC orders.
    char_id: Option<i64>,
    at: Station,
    price: f64,
    vol_entered: i32,
    vol_remaining: i32,
    min_volume: i32,
    bid: bool,
    range: i64,
    duration: i64,
    issued: i64,
    escrow: f64,
}

/// Selects every Order column; $1 is today (the issue date of NPC orders).
const ORDER_SELECT: &str = "SELECT o.order_id, o.type_id, o.char_id, o.station_id, o.solar_system_id, o.constellation_id, o.region_id,
        COALESCE(s.owner_id, 0)::bigint, o.price, o.vol_entered, o.vol_remaining, o.min_volume, o.bid,
        o.range::bigint, o.duration::bigint, COALESCE(o.issued, $1), o.escrow
    FROM game.market_orders o LEFT JOIN sde.sta_stations s ON s.station_id = o.station_id";

fn order_from_row(r: &tokio_postgres::Row) -> Order {
    Order {
        id: r.get(0),
        type_id: r.get(1),
        char_id: r.get(2),
        at: Station { id: r.get(3), system: r.get(4), constellation: r.get(5), region: r.get(6), owner: r.get(7) },
        price: r.get(8),
        vol_entered: r.get(9),
        vol_remaining: r.get(10),
        min_volume: r.get(11),
        bid: r.get(12),
        range: r.get(13),
        duration: r.get(14),
        issued: r.get(15),
        escrow: r.get(16),
    }
}

async fn my_order(db: &Db, char_id: i64, order_id: i64) -> Result<Order, CallError> {
    db.client
        .query_opt(&format!("{ORDER_SELECT} WHERE o.order_id = $2 AND o.char_id = $3"), &[&today(), &order_id, &char_id])
        .await
        .map_err(db_err)?
        .map(|r| order_from_row(&r))
        .ok_or_else(|| notify_error("该订单已关闭。"))
}

async fn insert_order(db: &Db, o: &mut Order) -> Result<(), CallError> {
    o.id = db
        .client
        .query_one(
            "INSERT INTO game.market_orders (type_id, char_id, station_id, solar_system_id, constellation_id, region_id, price,
                                             vol_entered, vol_remaining, min_volume, bid, range, duration, issued, escrow)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15) RETURNING order_id",
            &[
                &o.type_id,
                &o.char_id,
                &o.at.id,
                &o.at.system,
                &o.at.constellation,
                &o.at.region,
                &o.price,
                &o.vol_entered,
                &o.vol_remaining,
                &o.min_volume,
                &o.bid,
                &(o.range as i32),
                &(o.duration as i32),
                &o.issued,
                &o.escrow,
            ],
        )
        .await
        .map_err(db_err)?
        .get(0);
    Ok(())
}

/// Move a player's order to the history.
async fn close(db: &Db, o: &Order, state: i32) -> Result<(), CallError> {
    db.client
        .execute(
            "WITH gone AS (DELETE FROM game.market_orders WHERE order_id = $1 RETURNING *)
             INSERT INTO game.market_order_history (order_id, type_id, char_id, station_id, region_id, price, vol_entered, vol_remaining,
                                                    min_volume, bid, range, duration, issued, state, closed)
             SELECT order_id, type_id, char_id, station_id, region_id, price, vol_entered, $2::int, min_volume, bid, range, duration,
                    COALESCE(issued, $4::bigint), $3::int, $4::bigint
             FROM gone",
            &[&o.id, &o.vol_remaining, &state, &filetime_now()],
        )
        .await
        .map_err(db_err)?;
    Ok(())
}

/// Give a closed order's escrow back: a sell order's items, a buy order's ISK.
async fn refund(db: &Db, world: &World, o: &Order) -> Result<(), CallError> {
    let Some(owner) = o.char_id else { return Ok(()) };
    if o.bid {
        credit(db, world, owner, round2(o.escrow)).await
    } else if o.vol_remaining > 0 {
        deliver(db, world, owner, o.at.id, o.type_id, o.vol_remaining).await
    } else {
        Ok(())
    }
}

/// The client's order KeyVal (OnOwnOrdersChanged, BuyMultipleItems).
fn order_keyval(o: &Order) -> Value {
    keyval([
        ("orderID", Value::Int(o.id)),
        ("typeID", Value::Int(o.type_id as i64)),
        ("charID", o.char_id.map(Value::Int).unwrap_or(Value::None)),
        ("stationID", Value::Int(o.at.id)),
        ("solarSystemID", Value::Int(o.at.system)),
        ("regionID", Value::Int(o.at.region)),
        ("price", Value::Float(o.price)),
        ("range", Value::Int(o.range)),
        ("volEntered", Value::Int(o.vol_entered as i64)),
        ("volRemaining", Value::Int(o.vol_remaining as i64)),
        ("minVolume", Value::Int(o.min_volume as i64)),
        ("bid", Value::Bool(o.bid)),
        ("duration", Value::Int(o.duration)),
        ("issueDate", Value::Int(o.issued)),
    ])
}

fn tell_owner(world: &World, o: &Order, reason: &str) {
    if let Some(owner) = o.char_id {
        world.notify_pilot(owner, "OnOwnOrdersChanged", vec![Value::list([order_keyval(o)]), Value::str(reason), Value::Bool(false)]);
    }
}

/// `quantity` of a resting order traded: a player's order shrinks, and closes once filled.
async fn consume(db: &Db, world: &World, o: &mut Order, quantity: i32) -> Result<(), CallError> {
    let Some(owner) = o.char_id else { return Ok(()) };
    o.vol_remaining -= quantity;
    if o.bid {
        o.escrow = round2(o.escrow - o.price * quantity as f64).max(0.0);
    }
    if o.vol_remaining <= 0 {
        if o.bid {
            credit(db, world, owner, o.escrow).await?; // rounding left over
        }
        close(db, o, STATE_CLOSED).await?;
    } else {
        db.client
            .execute("UPDATE game.market_orders SET vol_remaining = $2, escrow = $3 WHERE order_id = $1", &[&o.id, &o.vol_remaining, &o.escrow])
            .await
            .map_err(db_err)?;
    }
    tell_owner(world, o, "Modify");
    Ok(())
}

/// Sell up to `quantity` x `type_id` at `at` to the best bids paying at least `floor`.
/// Returns how many sold.
async fn hit_bids(db: &Db, world: &World, seller: i64, at: &Station, type_id: i32, floor: f64, quantity: i32) -> Result<i32, CallError> {
    let g = graph(db).await?;
    let rows = db
        .client
        .query(
            &format!(
                "{ORDER_SELECT} WHERE o.type_id = $2 AND o.region_id = $3 AND o.bid AND o.vol_remaining > 0 AND o.price >= $4::float8 - 0.005
                 ORDER BY o.price DESC, o.issued NULLS FIRST, o.order_id"
            ),
            &[&today(), &type_id, &at.region, &floor],
        )
        .await
        .map_err(db_err)?;
    let tax = skills(db, seller).await?.sales_tax;
    let mut left = quantity;
    for r in &rows {
        if left == 0 {
            break;
        }
        let mut bid = order_from_row(r);
        if !in_range(g, bid.range, &bid.at, at) {
            continue;
        }
        let available = if bid.char_id.is_some() { bid.vol_remaining } else { left };
        let q = left.min(available);
        if q < bid.min_volume && available >= bid.min_volume {
            continue;
        }
        let value = round2(bid.price * q as f64);
        credit(db, world, seller, round2(value * (1.0 - tax))).await?;
        if let Some(buyer) = bid.char_id {
            deliver(db, world, buyer, at.id, type_id, q).await?;
        }
        record(db, type_id, q, bid.price, bid.char_id.unwrap_or(bid.at.owner), seller, at).await?;
        consume(db, world, &mut bid, q).await?;
        info!("market: {seller} sold {q} x {type_id} at {} for {value} ISK (order {})", at.id, bid.id);
        left -= q;
    }
    Ok(quantity - left)
}

/// Buy up to `quantity` x `type_id` from the cheapest asks within `range` of `at` at no more
/// than `cap`, paying from the wallet unless an order's escrow covers it.
/// Returns (bought, spent).
#[allow(clippy::too_many_arguments)]
async fn lift_asks(db: &Db, world: &World, buyer: i64, at: &Station, range: i64, type_id: i32, cap: f64, quantity: i32, from_wallet: bool) -> Result<(i32, f64), CallError> {
    let g = graph(db).await?;
    let rows = db
        .client
        .query(
            &format!(
                "{ORDER_SELECT} WHERE o.type_id = $2 AND o.region_id = $3 AND NOT o.bid AND o.vol_remaining > 0 AND o.price <= $4::float8 + 0.005
                 ORDER BY o.price, o.issued NULLS FIRST, o.order_id"
            ),
            &[&today(), &type_id, &at.region, &cap],
        )
        .await
        .map_err(db_err)?;
    let (mut left, mut spent) = (quantity, 0.0);
    for r in &rows {
        if left == 0 {
            break;
        }
        let mut ask = order_from_row(r);
        if !in_range(g, range, at, &ask.at) {
            continue;
        }
        let available = if ask.char_id.is_some() { ask.vol_remaining } else { left };
        let q = left.min(available);
        let value = round2(ask.price * q as f64);
        if from_wallet {
            match debit(db, world, buyer, value).await {
                Ok(()) => {}
                Err(CallError::User { .. }) if left < quantity => break,
                Err(e) => return Err(e),
            }
        }
        if let Some(seller) = ask.char_id {
            let tax = skills(db, seller).await?.sales_tax;
            credit(db, world, seller, round2(value * (1.0 - tax))).await?;
        }
        deliver(db, world, buyer, ask.at.id, type_id, q).await?;
        record(db, type_id, q, ask.price, buyer, ask.char_id.unwrap_or(ask.at.owner), &ask.at).await?;
        consume(db, world, &mut ask, q).await?;
        info!("market: {buyer} bought {q} x {type_id} at {} for {value} ISK (order {})", ask.at.id, ask.id);
        left -= q;
        spent += value;
    }
    Ok((quantity - left, spent))
}

/// `PlaceBuyOrder` / `BuyMultipleItems`: buy what the book offers now, and with a duration
/// leave the rest as a buy order. Returns the order as the client's KeyVal.
#[allow(clippy::too_many_arguments)]
async fn place_buy(c: &Client, station_id: i64, type_id: i64, price: f64, quantity: i64, range: i64, min_volume: i64, duration: i64) -> CallResult {
    let (db, world) = (&c.db, &*c.world);
    let me = c.session.int("charid").unwrap_or(0);
    let price = valid_price(price)?;
    if quantity <= 0 || quantity > i32::MAX as i64 || !(0..=MAX_DURATION).contains(&duration) {
        return Err(user_error("MktOrderDidNotMatch", vec![]));
    }
    let (type_id, quantity) = (type_id as i32, quantity as i32);
    let min_volume = (min_volume.clamp(1, quantity as i64)) as i32;
    let _trading = lock().await;
    let at = station(db, station_id).await?;
    let skills = skills(db, me).await?;
    if duration > 0 {
        check_order_slots(db, me, &skills).await?;
        let escrow = round2(price * quantity as f64);
        require_balance(db, me, escrow + broker_fee(skills.broker_fee, escrow)).await?;
    }
    let (bought, _) = lift_asks(db, world, me, &at, range, type_id, price, quantity, true).await?;
    let mut order = Order {
        id: 0,
        type_id,
        char_id: Some(me),
        at,
        price,
        vol_entered: quantity,
        vol_remaining: quantity - bought,
        min_volume,
        bid: true,
        range,
        duration,
        issued: filetime_now(),
        escrow: 0.0,
    };
    if duration == 0 || order.vol_remaining == 0 {
        if bought == 0 {
            return Err(user_error("MktOrderDidNotMatch", vec![]));
        }
        order.duration = 0;
        order.vol_remaining = 0;
        world.notify_pilot(me, "OnOwnOrdersChanged", vec![Value::list([order_keyval(&order)]), Value::str("Expiry"), Value::Bool(false)]);
        return Ok(order_keyval(&order));
    }
    order.escrow = round2(price * order.vol_remaining as f64);
    debit(db, world, me, order.escrow + broker_fee(skills.broker_fee, order.escrow)).await?;
    insert_order(db, &mut order).await?;
    info!("market: {me} placed buy order {} for {} x {type_id} at {price} ISK", order.id, order.vol_remaining);
    tell_owner(world, &order, "Add");
    Ok(order_keyval(&order))
}

/// One item of `PlaceMultiSellOrder`: sell to the bids now, and with a duration list the
/// rest as a sell order. Returns whether anything sold or was listed.
async fn place_sell(c: &Client, item_id: i64, station_id: i64, type_id: i64, price: f64, quantity: i64, duration: i64) -> Result<bool, CallError> {
    let (db, world) = (&c.db, &*c.world);
    let me = c.session.int("charid").unwrap_or(0);
    let price = valid_price(price)?;
    if quantity <= 0 || !(0..=MAX_DURATION).contains(&duration) {
        return Err(user_error("MktOrderDidNotMatch", vec![]));
    }
    let _trading = lock().await;
    let item = load_items_db(db, "i.item_id = $1", &[&item_id]).await?.pop().ok_or_else(|| notify_error("该物品已不在原处。"))?;
    if item.owner_id != me || item.location_id != station_id || item.flag_id != FLAG_HANGAR || item.type_id as i64 != type_id {
        return Err(notify_error("只能在物品所在空间站出售你物品机库中的物品。"));
    }
    if item.singleton {
        return Err(user_error("RepackageBeforeSelling", vec![("item".into(), Value::Int(type_id))]));
    }
    let (type_id, quantity) = (item.type_id, quantity.min(item.quantity as i64) as i32);
    let at = station(db, station_id).await?;
    let skills = skills(db, me).await?;
    if duration == 0 {
        let sold = hit_bids(db, world, me, &at, type_id, price, quantity).await?;
        if sold == 0 {
            return Err(user_error("MktOrderDidNotMatch", vec![]));
        }
        take_items(db, world, item, sold).await?;
        let done = Order {
            id: 0,
            type_id,
            char_id: Some(me),
            at,
            price,
            vol_entered: sold,
            vol_remaining: 0,
            min_volume: 1,
            bid: false,
            range: RANGE_STATION,
            duration: 0,
            issued: filetime_now(),
            escrow: 0.0,
        };
        world.notify_pilot(me, "OnOwnOrdersChanged", vec![Value::list([order_keyval(&done)]), Value::str("Expiry"), Value::Bool(false)]);
        return Ok(true);
    }
    check_order_slots(db, me, &skills).await?;
    require_balance(db, me, broker_fee(skills.broker_fee, price * quantity as f64)).await?;
    // The items go into escrow first; what the bids don't take stays listed.
    take_items(db, world, item, quantity).await?;
    let sold = hit_bids(db, world, me, &at, type_id, price, quantity).await?;
    let mut order = Order {
        id: 0,
        type_id,
        char_id: Some(me),
        at,
        price,
        vol_entered: quantity,
        vol_remaining: quantity - sold,
        min_volume: 1,
        bid: false,
        range: RANGE_STATION,
        duration,
        issued: filetime_now(),
        escrow: 0.0,
    };
    if order.vol_remaining == 0 {
        return Ok(true);
    }
    if let Err(e) = debit(db, world, me, broker_fee(skills.broker_fee, price * order.vol_remaining as f64)).await {
        deliver(db, world, me, at.id, type_id, order.vol_remaining).await?;
        return Err(e);
    }
    insert_order(db, &mut order).await?;
    info!("market: {me} placed sell order {} for {} x {type_id} at {price} ISK", order.id, order.vol_remaining);
    tell_owner(world, &order, "Add");
    Ok(true)
}

/// `ModifyCharOrder`: reprice an open order, paying the modification fee (and topping up or
/// releasing a buy order's escrow). A price that crosses the book trades at once.
async fn modify(c: &Client, order_id: i64, new_price: f64) -> CallResult {
    let (db, world) = (&c.db, &*c.world);
    let me = c.session.int("charid").unwrap_or(0);
    let new_price = valid_price(new_price)?;
    let _trading = lock().await;
    let mut o = my_order(db, me, order_id).await?;
    let skills = skills(db, me).await?;
    let volume = o.vol_remaining as f64;
    let (old_amount, new_amount) = (o.price * volume, new_price * volume);
    let fee = round2(
        (new_amount * skills.broker_fee * (1.0 - skills.modification_discount) + (new_amount - old_amount).max(0.0) * skills.broker_fee).max(MIN_BROKER_FEE),
    );
    let escrow = if o.bid { round2(new_amount) } else { 0.0 };
    let top_up = escrow - o.escrow;
    debit(db, world, me, fee + top_up.max(0.0)).await?;
    credit(db, world, me, round2(-top_up)).await?;
    o.price = new_price;
    o.escrow = escrow;
    o.issued = filetime_now();
    db.client
        .execute("UPDATE game.market_orders SET price = $2, escrow = $3, issued = $4 WHERE order_id = $1", &[&o.id, &o.price, &o.escrow, &o.issued])
        .await
        .map_err(db_err)?;
    info!("market: {me} repriced order {} to {new_price} ISK", o.id);
    let traded = if o.bid {
        let (bought, spent) = lift_asks(db, world, me, &o.at, o.range, o.type_id, o.price, o.vol_remaining, false).await?;
        // The escrow paid this order's price; cheaper asks leave change.
        credit(db, world, me, round2(o.price * bought as f64 - spent)).await?;
        bought
    } else {
        hit_bids(db, world, me, &o.at, o.type_id, o.price, o.vol_remaining).await?
    };
    if traded > 0 {
        consume(db, world, &mut o, traded).await?;
    } else {
        tell_owner(world, &o, "Modify");
    }
    Ok(Value::None)
}

/// `CancelCharOrder`: close an open order and hand back its escrow (the broker's fee stays paid).
async fn cancel(c: &Client, order_id: i64) -> CallResult {
    let (db, world) = (&c.db, &*c.world);
    let me = c.session.int("charid").unwrap_or(0);
    let _trading = lock().await;
    let o = my_order(db, me, order_id).await?;
    close(db, &o, STATE_CANCELLED).await?;
    refund(db, world, &o).await?;
    info!("market: {me} cancelled order {}", o.id);
    tell_owner(world, &o, "Cancel");
    Ok(Value::None)
}

/// Close players' orders past their duration, once a minute.
pub async fn expire_orders(db: Db, world: Arc<World>) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(ORDER_MAINTENANCE_INTERVAL_SECONDS)).await;
        if let Err(CallError::Internal(e)) = expire(&db, &world).await {
            warn!("expiring market orders failed: {e:#}");
        }
    }
}

async fn expire(db: &Db, world: &World) -> Result<(), CallError> {
    let _trading = lock().await;
    let rows = db
        .client
        .query(&format!("{ORDER_SELECT} WHERE o.char_id IS NOT NULL AND o.issued + o.duration::bigint * $2 <= $3"), &[&today(), &FILETIME_DAY, &filetime_now()])
        .await
        .map_err(db_err)?;
    for r in &rows {
        let o = order_from_row(r);
        close(db, &o, STATE_CLOSED).await?;
        refund(db, world, &o).await?;
        info!("market: order {} of {:?} expired", o.id, o.char_id);
        tell_owner(world, &o, "Expiry");
    }
    Ok(())
}

// ---------------------------------------------------------------- order lists

/// `GetCharOrders`: the character's open orders.
async fn my_orders(c: &Client) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let here = c.session.int("solarsystemid2").unwrap_or(0);
    let g = graph(&c.db).await?;
    let rows = c.db.client.query(&format!("{ORDER_SELECT} WHERE o.char_id = $2 ORDER BY o.order_id"), &[&today(), &me]).await.map_err(db_err)?;
    let header = DbRowDescriptor::new([
        ("orderID", DbType::I8),
        ("typeID", DbType::I4),
        ("charID", DbType::I8),
        ("regionID", DbType::I4),
        ("stationID", DbType::I8),
        ("solarSystemID", DbType::I4),
        ("constellationID", DbType::I4),
        ("range", DbType::I2),
        ("bid", DbType::Bool),
        ("price", DbType::R8),
        ("volEntered", DbType::I4),
        ("volRemaining", DbType::I4),
        ("issueDate", DbType::FileTime),
        ("minVolume", DbType::I4),
        ("duration", DbType::I2),
        ("isCorp", DbType::Bool),
        ("escrow", DbType::R8),
        ("keyID", DbType::I4),
        ("jumps", DbType::I4),
    ]);
    let lines = rows
        .iter()
        .map(|r| {
            let o = order_from_row(r);
            header.row(&[
                Value::Int(o.id),
                Value::Int(o.type_id as i64),
                Value::Int(me),
                Value::Int(o.at.region),
                Value::Int(o.at.id),
                Value::Int(o.at.system),
                Value::Int(o.at.constellation),
                Value::Int(o.range),
                Value::Bool(o.bid),
                Value::Float(o.price),
                Value::Int(o.vol_entered as i64),
                Value::Int(o.vol_remaining as i64),
                Value::Int(o.issued),
                Value::Int(o.min_volume as i64),
                Value::Int(o.duration),
                Value::Bool(false),
                Value::Float(o.escrow),
                Value::Int(ACCOUNT_CASH as i64),
                Value::Int(jumps(g, here, o.at.system, UNREACHABLE).unwrap_or(UNREACHABLE)),
            ])
        })
        .collect();
    Ok(crowset(&header, lines))
}

/// `GetMarketOrderHistory`: the character's closed orders of the last 90 days.
async fn order_history(c: &Client) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let rows = c
        .db
        .client
        .query(
            "SELECT order_id, type_id::bigint, region_id, station_id, range::bigint, bid, price, vol_entered::bigint, vol_remaining::bigint,
                    issued, min_volume::bigint, duration::bigint, state::bigint, closed
             FROM game.market_order_history WHERE char_id = $1 AND closed >= $2 ORDER BY closed DESC LIMIT 1000",
            &[&me, &(filetime_now() - ORDER_HISTORY_DAYS * FILETIME_DAY)],
        )
        .await
        .map_err(db_err)?;
    let header = DbRowDescriptor::new([
        ("orderID", DbType::I8),
        ("typeID", DbType::I4),
        ("charID", DbType::I8),
        ("regionID", DbType::I4),
        ("stationID", DbType::I8),
        ("range", DbType::I2),
        ("bid", DbType::Bool),
        ("price", DbType::R8),
        ("volEntered", DbType::I4),
        ("volRemaining", DbType::I4),
        ("issueDate", DbType::FileTime),
        ("minVolume", DbType::I4),
        ("duration", DbType::I2),
        ("orderState", DbType::I4),
        ("lastStateChange", DbType::FileTime),
        ("isCorp", DbType::Bool),
        ("keyID", DbType::I4),
    ]);
    let lines = rows
        .iter()
        .map(|r| {
            header.row(&[
                Value::Int(r.get(0)),
                Value::Int(r.get(1)),
                Value::Int(me),
                Value::Int(r.get(2)),
                Value::Int(r.get(3)),
                Value::Int(r.get(4)),
                Value::Bool(r.get(5)),
                Value::Float(r.get(6)),
                Value::Int(r.get(7)),
                Value::Int(r.get(8)),
                Value::Int(r.get(9)),
                Value::Int(r.get(10)),
                Value::Int(r.get(11)),
                Value::Int(r.get(12)),
                Value::Int(r.get(13)),
                Value::Bool(false),
                Value::Int(ACCOUNT_CASH as i64),
            ])
        })
        .collect();
    Ok(crowset(&header, lines))
}

/// `CharGetTransactions(fromDate)`: the character's fills, newest first (marketSvc.py).
async fn transactions(c: &Client, from: Option<i64>) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let from = from.unwrap_or(filetime_now() - DEFAULT_TRANSACTION_HISTORY_DAYS * FILETIME_DAY);
    let rows = c
        .db
        .client
        .query(
            "SELECT transaction_id, date, type_id::bigint, quantity::bigint, price, station_id, buyer_id, seller_id
             FROM game.market_transactions WHERE (buyer_id = $1 OR seller_id = $1) AND date >= $2 ORDER BY date DESC LIMIT 2500",
            &[&me, &from],
        )
        .await
        .map_err(db_err)?;
    let header = DbRowDescriptor::new([
        ("transactionID", DbType::I8),
        ("transactionDate", DbType::FileTime),
        ("typeID", DbType::I4),
        ("quantity", DbType::I4),
        ("price", DbType::R8),
        ("stationID", DbType::I8),
        ("buyerID", DbType::I8),
        ("sellerID", DbType::I8),
        ("accountID", DbType::I4),
        ("buyerAccountID", DbType::I4),
        ("sellerAccountID", DbType::I4),
    ]);
    let lines = rows
        .iter()
        .map(|r| {
            header.row(&[
                Value::Int(r.get(0)),
                Value::Int(r.get(1)),
                Value::Int(r.get(2)),
                Value::Int(r.get(3)),
                Value::Float(r.get(4)),
                Value::Int(r.get(5)),
                Value::Int(r.get(6)),
                Value::Int(r.get(7)),
                Value::Int(ACCOUNT_CASH as i64),
                Value::Int(ACCOUNT_CASH as i64),
                Value::Int(ACCOUNT_CASH as i64),
            ])
        })
        .collect();
    Ok(crowset(&header, lines))
}

/// `GetCharEscrow`: ISK held by buy orders, and the value of items held by sell orders.
async fn escrow(c: &Client) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let r = c
        .db
        .client
        .query_one(
            "SELECT COALESCE(sum(escrow) FILTER (WHERE bid), 0)::float8, COALESCE(sum(price * vol_remaining) FILTER (WHERE NOT bid), 0)::float8
             FROM game.market_orders WHERE char_id = $1",
            &[&me],
        )
        .await
        .map_err(db_err)?;
    Ok(keyval([("iskEscrow", Value::Float(r.get(0))), ("itemsEscrow", Value::Float(r.get(1)))]))
}

fn history_header() -> DbRowDescriptor {
    DbRowDescriptor::new([
        ("historyDate", DbType::FileTime),
        ("lowPrice", DbType::R8),
        ("highPrice", DbType::R8),
        ("avgPrice", DbType::R8),
        ("volume", DbType::I8),
        ("orders", DbType::I4),
    ])
}

/// Daily trading in the session's region for each type, from the year's transactions:
/// typeID -> (days before today, today) as `dbutil.CRowset`s (marketsvc.GetHistoryRowList).
async fn price_history(c: &Client, type_ids: &[i32]) -> Result<HashMap<i32, (Value, Value)>, CallError> {
    let region = c.session.int("regionid").unwrap_or(0);
    let today = today();
    let rows = c
        .db
        .client
        .query(
            "SELECT type_id, date / $3 * $3 AS day, min(price), max(price), sum(price * quantity) / sum(quantity), sum(quantity)::bigint, count(*)
             FROM game.market_transactions WHERE region_id = $1 AND type_id = ANY($2) AND date >= $4
             GROUP BY 1, 2 ORDER BY 1, 2",
            &[&region, &type_ids.to_vec(), &FILETIME_DAY, &(today - PRICE_HISTORY_DAYS * FILETIME_DAY)],
        )
        .await
        .map_err(db_err)?;
    let header = history_header();
    let mut days: HashMap<i32, (Vec<Value>, Vec<Value>)> = type_ids.iter().map(|t| (*t, (vec![], vec![]))).collect();
    for r in &rows {
        let (type_id, day): (i32, i64) = (r.get(0), r.get(1));
        let row = header.row(&[Value::Int(day), Value::Float(r.get(2)), Value::Float(r.get(3)), Value::Float(r.get(4)), Value::Int(r.get(5)), Value::Int(r.get(6))]);
        let (old, new) = days.entry(type_id).or_default();
        (if day < today { old } else { new }).push(row);
    }
    Ok(days.into_iter().map(|(t, (old, new))| (t, (crowset(&header, old), crowset(&header, new)))).collect())
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service != "marketProxy" {
        return None;
    }
    let int = |i: usize| call.args.get(i).and_then(|v| v.as_int());
    let float = |i: usize| call.args.get(i).and_then(|v| v.as_float()).unwrap_or(0.0);
    Some(match call.method {
        "GetOrders" => orders(c, int(0).unwrap_or(0)).await,
        "GetStationAsks" => {
            let station = c.session.int("stationid").unwrap_or(0);
            best_asks(c, "station_id", station, false).await
        }
        "GetSystemAsks" => {
            let system = c.session.int("solarsystemid2").unwrap_or(0);
            best_asks(c, "solar_system_id", system, false).await
        }
        "GetRegionBest" => {
            let region = c.session.int("regionid").unwrap_or(0);
            best_asks(c, "region_id", region, true).await
        }
        // PlaceBuyOrder(stationID, typeID, price, quantity, orderRange, minVolume, duration, useCorp, expectedBrokersFee)
        "PlaceBuyOrder" => {
            let (station, type_id, quantity) = (int(0).unwrap_or(0), int(1).unwrap_or(0), int(3).unwrap_or(0));
            let (range, min_volume, duration) = (int(4).unwrap_or(RANGE_STATION), int(5).unwrap_or(1), int(6).unwrap_or(0));
            place_buy(c, station, type_id, float(2), quantity, range, min_volume, duration).await.map(|_| Value::None)
        }
        // BuyMultipleItems(stationID, [KeyVal(stationID, typeID, price, quantity, ...)], useCorp) -> orders
        "BuyMultipleItems" => {
            let station = int(0).unwrap_or(0);
            let items = call.args.get(1).and_then(|v| v.items()).map(|v| v.to_vec()).unwrap_or_default();
            let mut done = vec![];
            for item in &items {
                let get = |k: &str| value_field(item, k);
                let type_id = get("typeID").and_then(|v| v.as_int()).unwrap_or(0);
                let price = get("price").and_then(|v| v.as_float()).unwrap_or(0.0);
                let quantity = get("quantity").and_then(|v| v.as_int()).unwrap_or(0);
                let at = get("stationID").and_then(|v| v.as_int()).unwrap_or(station);
                match place_buy(c, at, type_id, price, quantity, RANGE_STATION, 1, 0).await {
                    Ok(order) => done.push(order),
                    Err(e @ CallError::User { .. }) if items.len() == 1 => return Some(Err(e)),
                    Err(CallError::User { .. }) => {}
                    Err(e) => return Some(Err(e)),
                }
            }
            Ok(Value::List(done))
        }
        // PlaceMultiSellOrder([KeyVal(stationID, typeID, itemID, price, quantity, ...)], useCorp, duration, expectedBrokersFee)
        // -> whether any order was made (sellMulti closes its window on True)
        "PlaceMultiSellOrder" => {
            let items = call.args.first().and_then(|v| v.items()).map(|v| v.to_vec()).unwrap_or_default();
            let duration = int(2).unwrap_or(0);
            let mut any = false;
            for item in &items {
                let get = |k: &str| value_field(item, k).and_then(|v| v.as_int()).unwrap_or(0);
                let price = value_field(item, "price").and_then(|v| v.as_float()).unwrap_or(0.0);
                match place_sell(c, get("itemID"), get("stationID"), get("typeID"), price, get("quantity"), duration).await {
                    Ok(made) => any |= made,
                    Err(e @ CallError::User { .. }) if items.len() == 1 => return Some(Err(e)),
                    Err(CallError::User { msg, .. }) => warn!(user = c.login.user_id, "market: selling {} failed: {msg}", get("itemID")),
                    Err(e) => return Some(Err(e)),
                }
            }
            Ok(Value::Bool(any))
        }
        // ModifyCharOrder(orderID, newPrice, bid, stationID, solarSystemID, price, range, volRemaining, issueDate)
        "ModifyCharOrder" => modify(c, int(0).unwrap_or(0), float(1)).await,
        // CancelCharOrder(orderID, regionID)
        "CancelCharOrder" => cancel(c, int(0).unwrap_or(0)).await,
        "StartupCheck" => Ok(Value::None),
        "GetCharEscrow" => escrow(c).await,
        "GetPlexBest" => Ok(Value::Dict(vec![])),
        "GetPlexHistory" => Ok(Value::Dict(vec![])),
        // GetHistoryForManyTypeIDs({typeID}) -> {typeID: (old, new)}
        "GetHistoryForManyTypeIDs" => {
            let type_ids: Vec<i32> = call.args.first().and_then(|v| v.items()).unwrap_or(&[]).iter().filter_map(|v| v.as_int()).map(|t| t as i32).collect();
            price_history(c, &type_ids)
                .await
                .map(|h| Value::Dict(h.into_iter().map(|(t, (old, new))| (Value::Int(t as i64), Value::tuple([old, new]))).collect()))
        }
        "GetOldPriceHistory" | "GetNewPriceHistory" => {
            let type_id = int(0).unwrap_or(0) as i32;
            price_history(c, &[type_id]).await.map(|mut h| {
                let (old, new) = h.remove(&type_id).unwrap_or_else(|| (crowset(&history_header(), vec![]), crowset(&history_header(), vec![])));
                if call.method == "GetOldPriceHistory" {
                    old
                } else {
                    new
                }
            })
        }
        "GetPlexOldPriceHistory" | "GetPlexNewPriceHistory" => Ok(crowset(&history_header(), vec![])),
        "CharGetTransactions" => transactions(c, int(0)).await,
        "CorpGetTransactions" => Ok(Value::list([])),
        "GetCharOrders" => my_orders(c).await,
        "GetCorporationOrders" => Ok(Value::list([])),
        "GetMarketOrderHistory" => order_history(c).await,
        _ => return None,
    })
}
