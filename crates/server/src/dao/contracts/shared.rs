//! Contract item, bid and status persistence for contractProxy.
//! Uses game contracts, contract items, bids and items; policy stays in services.
use crate::db::{executor::Executor, Error};
use tokio_postgres::Row;

pub(crate) struct ContractItem { pub contract_id: i64, pub item_id: Option<i64>, pub type_id: i64, pub quantity: i64, pub in_crate: bool }
impl ContractItem { fn from_row(r: &Row) -> Self { Self { contract_id: r.get("contract_id"), item_id: r.get("item_id"), type_id: r.get("type_id"), quantity: r.get("quantity"), in_crate: r.get("in_crate") } } }
pub(crate) async fn items(ex: &(impl Executor + ?Sized), ids: &[i64]) -> Result<Vec<ContractItem>, Error> {
    Ok(ex.query("SELECT contract_id, item_id, type_id::bigint, quantity::bigint, in_crate FROM game.contract_items WHERE contract_id = ANY($1)", &[&ids]).await?.iter().map(ContractItem::from_row).collect())
}
pub(crate) struct Bid { pub bid_id: i64, pub contract_id: i64, pub bidder_id: i64, pub amount: f64, pub date: i64 }
impl Bid { fn from_row(r: &Row) -> Self { Self { bid_id: r.get("bid_id"), contract_id: r.get("contract_id"), bidder_id: r.get("bidder_id"), amount: r.get("amount"), date: r.get("date") } } }
pub(crate) async fn bids(ex: &(impl Executor + ?Sized), ids: &[i64]) -> Result<Vec<Bid>, Error> {
    Ok(ex.query("SELECT bid_id, contract_id, bidder_id, amount, date FROM game.contract_bids WHERE contract_id = ANY($1) ORDER BY amount DESC", &[&ids]).await?.iter().map(Bid::from_row).collect())
}
pub(crate) async fn set_status(ex: &(impl Executor + ?Sized), id: i64, status: i32, completed: i64, acceptor: i64) -> Result<u64, Error> {
    ex.execute("UPDATE game.contracts SET status = $2, date_completed = $3, acceptor_id = $4 WHERE contract_id = $1", &[&id, &status, &completed, &acceptor]).await
}
struct ItemId { item_id: i64 }
impl ItemId { fn from_row(r: &Row) -> Self { Self { item_id: r.get("item_id") } } }
pub(crate) async fn move_item(ex: &(impl Executor + ?Sized), id: i64, location: i64) -> Result<u64, Error> {
    ex.execute("UPDATE game.items SET location_id = $2 WHERE item_id = $1", &[&id, &location]).await
}
pub(crate) async fn insert_item(ex: &(impl Executor + ?Sized), contract: i64, item: i64, type_id: i32, quantity: i32) -> Result<u64, Error> {
    ex.execute("INSERT INTO game.contract_items (contract_id, item_id, type_id, quantity, in_crate) VALUES ($1, $2, $3, $4, true)", &[&contract, &item, &type_id, &quantity]).await
}
pub(crate) async fn escrowed_ids(ex: &(impl Executor + ?Sized), id: i64) -> Result<Vec<i64>, Error> {
    Ok(ex.query("SELECT item_id FROM game.contract_items WHERE contract_id = $1 AND in_crate AND item_id IS NOT NULL", &[&id]).await?.iter().map(|r| ItemId::from_row(r).item_id).collect())
}
pub(crate) async fn destroy_escrow(ex: &(impl Executor + ?Sized), ids: &[i64]) -> Result<(), Error> {
    ex.execute("DELETE FROM game.items WHERE location_id = ANY($1)", &[&ids]).await?;
    ex.execute("DELETE FROM game.items WHERE item_id = ANY($1)", &[&ids]).await?;
    Ok(())
}
pub(crate) struct TopBid { pub bidder_id: i64, pub amount: f64 }
impl TopBid { fn from_row(r: &Row) -> Self { Self { bidder_id: r.get("bidder_id"), amount: r.get("amount") } } }
pub(crate) async fn top_bid(ex: &(impl Executor + ?Sized), id: i64) -> Result<Option<TopBid>, Error> {
    Ok(ex.query_opt("SELECT bidder_id, amount FROM game.contract_bids WHERE contract_id = $1 ORDER BY amount DESC, bid_id LIMIT 1", &[&id]).await?.map(|r| TopBid::from_row(&r)))
}
struct Count { count: i64 }
impl Count { fn from_row(r: &Row) -> Self { Self { count: r.get("count") } } }
pub(crate) async fn num_outstanding(ex: &(impl Executor + ?Sized), id: i64, status: i32) -> Result<i64, Error> {
    Ok(Count::from_row(&ex.query_one("SELECT count(*) FROM game.contracts WHERE issuer_id = $1 AND status = $2", &[&id, &status]).await?).count)
}
