//! Market order writes and slot counts for marketProxy
//! (`eve/client/script/ui/services/marketsvc.py`), using game.market_orders,
//! game.market_order_history and game.market_transactions. Matching, transactions,
//! notifications and refusals remain in the service.

use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

struct OrderCount { count: i64 }
impl OrderCount {
    fn from_row(row: &Row) -> Self { Self { count: row.get("count") } }
}

pub(crate) async fn order_count(ex: &(impl Executor + ?Sized), char_id: i64) -> Result<i64, db::Error> {
    Ok(OrderCount::from_row(&ex.query_one("SELECT count(*) FROM game.market_orders WHERE char_id = $1", &[&char_id]).await?).count)
}

pub(crate) async fn record(ex: &(impl Executor + ?Sized), type_id: i32, quantity: i32, price: f64, buyer: i64, seller: i64, station: i64, region: i64, now: i64) -> Result<u64, db::Error> {
    ex.execute(
        "INSERT INTO game.market_transactions (type_id, quantity, price, buyer_id, seller_id, station_id, region_id, date)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        &[&type_id, &quantity, &price, &buyer, &seller, &station, &region, &now],
    ).await
}

pub(crate) async fn close(ex: &(impl Executor + ?Sized), id: i64, remaining: i32, state: i32, now: i64) -> Result<u64, db::Error> {
    ex.execute(
        "WITH gone AS (DELETE FROM game.market_orders WHERE order_id = $1 RETURNING *)
             INSERT INTO game.market_order_history (order_id, type_id, char_id, station_id, region_id, price, vol_entered, vol_remaining,
                                                    min_volume, bid, range, duration, issued, state, closed)
             SELECT order_id, type_id, char_id, station_id, region_id, price, vol_entered, $2::int, min_volume, bid, range, duration,
                    COALESCE(issued, $4::bigint), $3::int, $4::bigint
             FROM gone",
        &[&id, &remaining, &state, &now],
    ).await
}

pub(crate) async fn consume(ex: &(impl Executor + ?Sized), id: i64, remaining: i32, escrow: f64) -> Result<u64, db::Error> {
    ex.execute("UPDATE game.market_orders SET vol_remaining = $2, escrow = $3 WHERE order_id = $1", &[&id, &remaining, &escrow]).await
}

pub(crate) async fn reprice(ex: &(impl Executor + ?Sized), id: i64, price: f64, escrow: f64, issued: i64) -> Result<u64, db::Error> {
    ex.execute("UPDATE game.market_orders SET price = $2, escrow = $3, issued = $4 WHERE order_id = $1", &[&id, &price, &escrow, &issued]).await
}
