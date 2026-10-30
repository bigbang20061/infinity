//! Order storage for marketProxy (`eve/client/script/ui/services/marketsvc.py`),
//! using game.market_orders. Station owner enrichment, matching, transactions,
//! notifications and refusals are handled by the caller.
use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

#[derive(Clone, Copy)]
pub(crate) struct Station {
    pub id: i64, pub system: i64, pub constellation: i64, pub region: i64, pub owner: i64,
}
#[derive(Clone)]
pub(crate) struct Order {
    pub id: i64, pub type_id: i32, pub char_id: Option<i64>, pub at: Station,
    pub price: f64, pub vol_entered: i32, pub vol_remaining: i32, pub min_volume: i32,
    pub bid: bool, pub range: i64, pub duration: i64, pub issued: i64, pub escrow: f64,
}
impl Order {
    fn from_row(r: &Row) -> Self {
        Self { id: r.get("order_id"), type_id: r.get("type_id"), char_id: r.get("char_id"),
            at: Station { id: r.get("station_id"), system: r.get("solar_system_id"), constellation: r.get("constellation_id"), region: r.get("region_id"), owner: 0 },
            price: r.get("price"), vol_entered: r.get("vol_entered"), vol_remaining: r.get("vol_remaining"), min_volume: r.get("min_volume"), bid: r.get("bid"), range: r.get("range"), duration: r.get("duration"), issued: r.get("issued"), escrow: r.get("escrow") }
    }
}
const ORDER_SELECT: &str = "SELECT o.order_id, o.type_id, o.char_id, o.station_id, o.solar_system_id, o.constellation_id, o.region_id,
        o.price, o.vol_entered, o.vol_remaining, o.min_volume, o.bid,
        o.range::bigint, o.duration::bigint, COALESCE(o.issued, $1) AS issued, o.escrow
    FROM game.market_orders o";

pub(crate) async fn my_order(ex: &(impl Executor + ?Sized), char_id: i64, order_id: i64, now: i64) -> Result<Option<Order>, db::Error> {
    Ok(ex.query_opt(&format!("{ORDER_SELECT} WHERE o.order_id = $2 AND o.char_id = $3"), &[&now, &order_id, &char_id]).await?.as_ref().map(Order::from_row))
}
pub(crate) async fn my_orders(ex: &(impl Executor + ?Sized), char_id: i64, now: i64) -> Result<Vec<Order>, db::Error> {
    Ok(ex.query(&format!("{ORDER_SELECT} WHERE o.char_id = $2 ORDER BY o.order_id"), &[&now, &char_id]).await?.iter().map(Order::from_row).collect())
}
pub(crate) async fn expired(ex: &(impl Executor + ?Sized), today: i64, day: i64, now: i64) -> Result<Vec<Order>, db::Error> {
    Ok(ex.query(&format!("{ORDER_SELECT} WHERE o.char_id IS NOT NULL AND o.issued + o.duration::bigint * $2 <= $3"), &[&today, &day, &now]).await?.iter().map(Order::from_row).collect())
}
pub(crate) async fn bids(ex: &(impl Executor + ?Sized), today: i64, type_id: i32, region: i64, floor: f64) -> Result<Vec<Order>, db::Error> {
    Ok(ex.query(&format!("{ORDER_SELECT} WHERE o.type_id = $2 AND o.region_id = $3 AND o.bid AND o.vol_remaining > 0 AND o.price >= $4::float8 - 0.005
                 ORDER BY o.price DESC, o.issued NULLS FIRST, o.order_id"), &[&today, &type_id, &region, &floor]).await?.iter().map(Order::from_row).collect())
}
pub(crate) async fn asks(ex: &(impl Executor + ?Sized), today: i64, type_id: i32, region: i64, cap: f64) -> Result<Vec<Order>, db::Error> {
    Ok(ex.query(&format!("{ORDER_SELECT} WHERE o.type_id = $2 AND o.region_id = $3 AND NOT o.bid AND o.vol_remaining > 0 AND o.price <= $4::float8 + 0.005
                 ORDER BY o.price, o.issued NULLS FIRST, o.order_id"), &[&today, &type_id, &region, &cap]).await?.iter().map(Order::from_row).collect())
}
struct OrderId { id: i64 }
impl OrderId { fn from_row(r: &Row) -> Self { Self { id: r.get("order_id") } } }
pub(crate) async fn insert(ex: &(impl Executor + ?Sized), o: &Order) -> Result<i64, db::Error> {
    Ok(OrderId::from_row(&ex.query_one(
        "INSERT INTO game.market_orders (type_id, char_id, station_id, solar_system_id, constellation_id, region_id, price,
                                             vol_entered, vol_remaining, min_volume, bid, range, duration, issued, escrow)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15) RETURNING order_id",
        &[&o.type_id, &o.char_id, &o.at.id, &o.at.system, &o.at.constellation, &o.at.region, &o.price, &o.vol_entered, &o.vol_remaining, &o.min_volume, &o.bid, &(o.range as i32), &(o.duration as i32), &o.issued, &o.escrow],
    ).await?).id)
}
