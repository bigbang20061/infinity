//! Contract lifecycle persistence for contractProxy (eve/client/script/ui/shared/neocom/contracts).
//! Uses game contracts, contract items, bids and items, plus SDE item volumes.
//! Transaction management and contract policy remain in the service.

use crate::db::{Error, executor::Executor};
use tokio_postgres::Row;

pub(crate) struct NewContract<'a> {
    pub kind: i32,
    pub issuer: i64,
    pub issuer_corp: i64,
    pub assignee: i64,
    pub start: i64,
    pub end: i64,
    pub price: f64,
    pub reward: f64,
    pub collateral: f64,
    pub title: &'a str,
    pub description: &'a str,
    pub num_days: i32,
    pub status: i32,
    pub issued: i64,
    pub expires: i64,
}

struct ContractId { contract_id: i64 }
impl ContractId {
    fn from_row(row: &Row) -> Self { Self { contract_id: row.get("contract_id") } }
}

pub(crate) async fn insert(ex: &(impl Executor + ?Sized), c: &NewContract<'_>) -> Result<i64, Error> {
    let row = ex.query_one(
        "INSERT INTO game.contracts (type, issuer_id, issuer_corp_id, assignee_id, start_station_id, end_station_id, price, reward,
                                             collateral, title, description, num_days, status, date_issued, date_expired)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15) RETURNING contract_id",
        &[&c.kind, &c.issuer, &c.issuer_corp, &c.assignee, &c.start, &c.end, &c.price, &c.reward, &c.collateral, &c.title, &c.description, &c.num_days, &c.status, &c.issued, &c.expires],
    ).await?;
    Ok(ContractId::from_row(&row).contract_id)
}

pub(crate) async fn insert_requested_item(ex: &(impl Executor + ?Sized), id: i64, type_id: i32, quantity: i32) -> Result<u64, Error> {
    ex.execute("INSERT INTO game.contract_items (contract_id, type_id, quantity, in_crate) VALUES ($1, $2, $3, false)", &[&id, &type_id, &quantity]).await
}

pub(crate) async fn update_volume(ex: &(impl Executor + ?Sized), id: i64) -> Result<u64, Error> {
    ex.execute("UPDATE game.contracts SET volume = (SELECT COALESCE(sum(t.volume * ci.quantity), 0) FROM game.contract_items ci
                                                     JOIN sde.inv_types t USING (type_id) WHERE ci.contract_id = $1 AND ci.in_crate)
                 WHERE contract_id = $1", &[&id]).await
}

pub(crate) struct RequestedItem { pub type_id: i32, pub quantity: i32 }
impl RequestedItem {
    fn from_row(row: &Row) -> Self { Self { type_id: row.get("type_id"), quantity: row.get("quantity") } }
}

pub(crate) async fn requested_items(ex: &(impl Executor + ?Sized), id: i64) -> Result<Vec<RequestedItem>, Error> {
    Ok(ex.query("SELECT type_id, quantity FROM game.contract_items WHERE contract_id = $1 AND NOT in_crate", &[&id]).await?.iter().map(RequestedItem::from_row).collect())
}

pub(crate) async fn set_date_accepted(ex: &(impl Executor + ?Sized), id: i64, now: i64) -> Result<u64, Error> {
    ex.execute("UPDATE game.contracts SET date_accepted = $2 WHERE contract_id = $1", &[&id, &now]).await
}

struct ItemId { item_id: i64 }
impl ItemId {
    fn from_row(row: &Row) -> Self { Self { item_id: row.get("item_id") } }
}

pub(crate) async fn insert_package(ex: &(impl Executor + ?Sized), type_id: i32, owner: i64, location: i64, flag: i32, custom_info: &str) -> Result<i64, Error> {
    let row = ex.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton, custom_info)
                             VALUES ($1, $2, $3, $4, 1, true, $5) RETURNING item_id", &[&type_id, &owner, &location, &flag, &custom_info]).await?;
    Ok(ItemId::from_row(&row).item_id)
}

pub(crate) async fn accept_courier(ex: &(impl Executor + ?Sized), id: i64, acceptor: i64, now: i64, status: i32, package: i64) -> Result<u64, Error> {
    ex.execute("UPDATE game.contracts SET acceptor_id = $2, date_accepted = $3, status = $4, package_id = $5 WHERE contract_id = $1", &[&id, &acceptor, &now, &status, &package]).await
}

pub(crate) async fn insert_bid(ex: &(impl Executor + ?Sized), id: i64, bidder: i64, amount: f64, now: i64) -> Result<u64, Error> {
    ex.execute("INSERT INTO game.contract_bids (contract_id, bidder_id, amount, date) VALUES ($1, $2, $3, $4)", &[&id, &bidder, &amount, &now]).await
}

pub(crate) async fn expired_auctions_with_bids(ex: &(impl Executor + ?Sized), kind: i32, status: i32, now: i64) -> Result<Vec<super::Contract>, Error> {
    let where_sql = "c.type = $1 AND c.status = $2 AND c.date_expired <= $3 AND EXISTS (SELECT 1 FROM game.contract_bids b WHERE b.contract_id = c.contract_id)";
    let rows = ex.query(&format!("{} WHERE {where_sql} ORDER BY c.contract_id DESC LIMIT 1000", super::CONTRACT_SELECT), &[&kind, &status, &now]).await?;
    Ok(rows.iter().map(super::Contract::from_row).collect())
}
