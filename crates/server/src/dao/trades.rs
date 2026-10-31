//! Direct-trade records in `game.trades` and `game.trade_items`.
//! The client contract is implemented by `services/trade`; this DAO does not
//! own transactions, reservation policy, notifications or timeout scheduling.
use crate::db::{executor::Executor, Error};
use tokio_postgres::Row;

#[derive(Clone)]
pub(crate) struct Trade {
    pub(crate) id: i64,
    pub(crate) station: i64,
    pub(crate) a: i64,
    pub(crate) b: i64,
    pub(crate) money_a: f64,
    pub(crate) money_b: f64,
    pub(crate) accepted_a: bool,
    pub(crate) accepted_b: bool,
    pub(crate) state: String,
}
impl Trade {
    fn from_row(row: &Row) -> Self {
        Self { id: row.get("trade_id"), station: row.get("station_id"), a: row.get("char_a"), b: row.get("char_b"), money_a: row.get("money_a"), money_b: row.get("money_b"), accepted_a: row.get("accepted_a"), accepted_b: row.get("accepted_b"), state: row.get("state") }
    }
}

#[derive(Clone)]
pub(crate) struct TradeItem {
    pub(crate) id: i64,
    pub(crate) owner: i64,
    pub(crate) origin: i64,
    pub(crate) flag: i32,
}
impl TradeItem {
    fn from_row(row: &Row) -> Self {
        Self { id: row.get("item_id"), owner: row.get("owner_id"), origin: row.get("origin_location"), flag: row.get("origin_flag") }
    }
}

const COLUMNS: &str = "trade_id, station_id, char_a, char_b, money_a::float8 AS money_a, money_b::float8 AS money_b, accepted_a, accepted_b, state";

pub(crate) async fn get(ex: &(impl Executor + ?Sized), id: i64) -> Result<Option<Trade>, Error> {
    Ok(ex.query_opt(&format!("SELECT {COLUMNS} FROM game.trades WHERE trade_id = $1"), &[&id]).await?.as_ref().map(Trade::from_row))
}

pub(crate) async fn open_for(ex: &(impl Executor + ?Sized), char_id: i64) -> Result<Option<Trade>, Error> {
    Ok(ex.query_opt(&format!("SELECT {COLUMNS} FROM game.trades WHERE state = 'open' AND (char_a = $1 OR char_b = $1)"), &[&char_id]).await?.as_ref().map(Trade::from_row))
}

pub(crate) async fn open(ex: &(impl Executor + ?Sized)) -> Result<Vec<Trade>, Error> {
    Ok(ex.query(&format!("SELECT {COLUMNS} FROM game.trades WHERE state = 'open' ORDER BY trade_id"), &[]).await?.iter().map(Trade::from_row).collect())
}

pub(crate) async fn create(ex: &(impl Executor + ?Sized), station: i64, a: i64, b: i64, now: i64) -> Result<Trade, Error> {
    Ok(Trade::from_row(&ex.query_one(&format!("INSERT INTO game.trades (station_id, char_a, char_b, state, created_at, updated_at) VALUES ($1, $2, $3, 'open', $4, $4) RETURNING {COLUMNS}"), &[&station, &a, &b, &now]).await?))
}

pub(crate) async fn items(ex: &(impl Executor + ?Sized), id: i64) -> Result<Vec<TradeItem>, Error> {
    Ok(ex.query("SELECT item_id, owner_id, origin_location, origin_flag FROM game.trade_items WHERE trade_id = $1 ORDER BY item_id", &[&id]).await?.iter().map(TradeItem::from_row).collect())
}

pub(crate) async fn add_item(ex: &(impl Executor + ?Sized), trade: i64, item: &TradeItem) -> Result<(), Error> {
    ex.execute("INSERT INTO game.trade_items (item_id, trade_id, owner_id, origin_location, origin_flag) VALUES ($1, $2, $3, $4, $5)", &[&item.id, &trade, &item.owner, &item.origin, &item.flag]).await?;
    Ok(())
}

pub(crate) async fn update(ex: &(impl Executor + ?Sized), trade: &Trade, now: i64) -> Result<bool, Error> {
    Ok(ex.execute("UPDATE game.trades SET money_a = $2::float8::numeric, money_b = $3::float8::numeric, accepted_a = $4, accepted_b = $5, state = $6, updated_at = $7 WHERE trade_id = $1 AND state = 'open'", &[&trade.id, &trade.money_a, &trade.money_b, &trade.accepted_a, &trade.accepted_b, &trade.state, &now]).await? == 1)
}

pub(crate) async fn clear_items(ex: &(impl Executor + ?Sized), trade: i64) -> Result<(), Error> {
    ex.execute("DELETE FROM game.trade_items WHERE trade_id = $1", &[&trade]).await?;
    Ok(())
}

/// Return the supplied IDs whose ancestor chain reaches an open trade container.
pub(crate) async fn reserved(ex: &(impl Executor + ?Sized), ids: &[i64]) -> Result<Vec<i64>, Error> {
    let rows = ex.query(
        "WITH RECURSIVE ancestors(origin, id) AS (SELECT id, id FROM unnest($1::int8[]) AS ids(id) UNION SELECT a.origin, i.location_id FROM ancestors a JOIN game.items i ON i.item_id = a.id) SELECT DISTINCT a.origin FROM ancestors a JOIN game.trades t ON t.trade_id = a.id AND t.state = 'open' ORDER BY a.origin",
        &[&ids],
    ).await?;
    Ok(rows.iter().map(|row| row.get("origin")).collect())
}

/// Move a validated root into or out of the trade container.
pub(crate) async fn relocate(ex: &(impl Executor + ?Sized), id: i64, owner: i64, from: i64, to: i64, flag: i32) -> Result<bool, Error> {
    Ok(ex.execute("UPDATE game.items SET location_id = $4, flag_id = $5 WHERE item_id = $1 AND owner_id = $2 AND location_id = $3", &[&id, &owner, &from, &to, &flag]).await? == 1)
}

/// Root IDs and nested contents, ordered for deterministic notifications.
pub(crate) async fn descendants(ex: &(impl Executor + ?Sized), ids: &[i64]) -> Result<Vec<i64>, Error> {
    let rows = ex.query("WITH RECURSIVE tree(id) AS (SELECT item_id FROM game.items WHERE item_id = ANY($1) UNION SELECT i.item_id FROM game.items i JOIN tree t ON i.location_id = t.id) SELECT id FROM tree ORDER BY id", &[&ids]).await?;
    Ok(rows.iter().map(|row| row.get("id")).collect())
}
