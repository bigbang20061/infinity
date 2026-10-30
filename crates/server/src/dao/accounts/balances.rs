//! PLEX vault balance lookup in `game.accounts` for store and gateway callers.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Unit conversion, transactions, refusals, notifications and World are caller concerns.
//! PLEX is stored as exact numeric and projected to clamped whole units for the client.
//! Exact GM writes use strings without a decimal dependency.
use crate::db::{self, executor::Executor};
use crate::consts::CLIENT_MAX_PLEX;
use tokio_postgres::Row;
pub(in crate::dao) struct Balance { pub(in crate::dao) plex_balance: i64 }
impl Balance { pub(in crate::dao) fn from_row(r: &Row) -> Self { Self { plex_balance: r.get("plex_balance") } } }

struct ExactBalance { exact: String, plex_balance: i64 }
impl ExactBalance {
    fn from_row(r: &Row) -> Self {
        Self { exact: r.get("exact"), plex_balance: Balance::from_row(r).plex_balance }
    }
}
pub(crate) async fn plex_balance(ex: &(impl Executor + ?Sized), user_id: i64) -> Result<i64, db::Error> {
    Ok(Balance::from_row(&ex.query_one("SELECT LEAST(plex_balance, $2::bigint)::bigint AS plex_balance FROM game.accounts WHERE user_id = $1", &[&user_id, &CLIENT_MAX_PLEX]).await?).plex_balance)
}

pub(crate) async fn set_plex(ex: &(impl Executor + ?Sized), user_id: i64, count: &str) -> Result<(String, i64), db::Error> {
    let row = ex.query_one(
        "UPDATE game.accounts SET plex_balance = $2::text::numeric WHERE user_id = $1 RETURNING plex_balance::text AS exact, LEAST(plex_balance, $3::bigint)::bigint AS plex_balance",
        &[&user_id, &count, &CLIENT_MAX_PLEX],
    ).await?;
    let balance = ExactBalance::from_row(&row);
    Ok((balance.exact, balance.plex_balance))
}

pub(crate) async fn add_plex(ex: &(impl Executor + ?Sized), user_id: i64, count: &str) -> Result<(String, i64), db::Error> {
    let row = ex.query_one(
        "UPDATE game.accounts SET plex_balance = plex_balance + $2::text::numeric WHERE user_id = $1 RETURNING plex_balance::text AS exact, LEAST(plex_balance, $3::bigint)::bigint AS plex_balance",
        &[&user_id, &count, &CLIENT_MAX_PLEX],
    ).await?;
    let balance = ExactBalance::from_row(&row);
    Ok((balance.exact, balance.plex_balance))
}
