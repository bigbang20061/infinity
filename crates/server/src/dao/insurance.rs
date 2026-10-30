//! Insurance persistence for insuranceSvc (eve/client/script/ui/station/insurance).
//! Uses game.insurance and game.retail_prices with sde.inv_types; policy and notifications
//! remain in the service. Corporate insurance is not supported there.

use crate::db::{executor::Executor, Error};

pub(crate) struct Contract {
    pub ship_id: i64,
    pub owner_id: i64,
    pub fraction: f64,
    pub start_date: i64,
    pub end_date: i64,
}
impl Contract {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self { ship_id: row.get("ship_id"), owner_id: row.get("owner_id"), fraction: row.get("fraction"), start_date: row.get("start_date"), end_date: row.get("end_date") }
    }
}
pub(crate) struct Payout {
    pub owner_id: i64,
    pub type_id: i32,
    pub fraction: f64,
    pub end_date: i64,
}
impl Payout {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self { owner_id: row.get("owner_id"), type_id: row.get("type_id"), fraction: row.get("fraction"), end_date: row.get("end_date") }
    }
}
struct Price { value: f64 }
impl Price {
    fn from_row(row: &tokio_postgres::Row) -> Self { Self { value: row.get("value") } }
}
const CONTRACT_SELECT: &str = "SELECT ship_id, owner_id, fraction, start_date, end_date FROM game.insurance";

pub(crate) async fn value(ex: &(impl Executor + ?Sized), type_id: i32, category: i32) -> Result<Option<f64>, Error> {
    Ok(ex.query_opt("SELECT COALESCE(NULLIF(r.adjusted, 0), NULLIF(r.average, 0), t.base_price, 0)::float8 AS value
             FROM sde.inv_types t LEFT JOIN game.retail_prices r USING (type_id) WHERE t.type_id = $1 AND t.category_id = $2", &[&type_id, &category]).await?.map(|r| Price::from_row(&r).value))
}
pub(crate) async fn contract(ex: &(impl Executor + ?Sized), ship: i64) -> Result<Option<Contract>, Error> {
    Ok(ex.query_opt(&format!("{CONTRACT_SELECT} WHERE ship_id = $1"), &[&ship]).await?.map(|r| Contract::from_row(&r)))
}
pub(crate) async fn contracts(ex: &(impl Executor + ?Sized), owner: i64, now: i64) -> Result<Vec<Contract>, Error> {
    Ok(ex.query(&format!("{CONTRACT_SELECT} WHERE owner_id = $1 AND end_date > $2"), &[&owner, &now]).await?.iter().map(Contract::from_row).collect())
}
pub(crate) async fn active_contract(ex: &(impl Executor + ?Sized), ship: i64, now: i64) -> Result<Option<Contract>, Error> {
    Ok(ex.query_opt(&format!("{CONTRACT_SELECT} WHERE ship_id = $1 AND end_date > $2"), &[&ship, &now]).await?.map(|r| Contract::from_row(&r)))
}
pub(crate) async fn insure(ex: &(impl Executor + ?Sized), ship: i64, owner: i64, type_id: i32, fraction: f64, start: i64, end: i64) -> Result<u64, Error> {
    ex.execute("INSERT INTO game.insurance (ship_id, owner_id, type_id, fraction, start_date, end_date) VALUES ($1, $2, $3, $4, $5, $6)
                 ON CONFLICT (ship_id) DO UPDATE SET owner_id = $2, type_id = $3, fraction = $4, start_date = $5, end_date = $6", &[&ship, &owner, &type_id, &fraction, &start, &end]).await
}
pub(crate) async fn take_payout(ex: &(impl Executor + ?Sized), ship: i64) -> Result<Option<Payout>, Error> {
    Ok(ex.query_opt("DELETE FROM game.insurance WHERE ship_id = $1 RETURNING owner_id, type_id, fraction, end_date", &[&ship]).await?.map(|r| Payout::from_row(&r)))
}
pub(crate) async fn uninsure(ex: &(impl Executor + ?Sized), ship: i64, owner: i64) -> Result<u64, Error> {
    ex.execute("DELETE FROM game.insurance WHERE ship_id = $1 AND owner_id = $2", &[&ship, &owner]).await
}
