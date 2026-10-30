//! PLEX vault balance lookup in `game.accounts` for store and gateway callers.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Unit conversion, transactions, refusals, notifications and World are caller concerns.
//! PLEX is stored as exact numeric and projected to clamped whole units for the client.
use crate::db::{self, executor::Executor};
use crate::consts::CLIENT_MAX_PLEX;
use tokio_postgres::Row;
pub(in crate::dao) struct Balance { pub(in crate::dao) plex_balance: i64 }
impl Balance { pub(in crate::dao) fn from_row(r: &Row) -> Self { Self { plex_balance: r.get("plex_balance") } } }
pub(crate) async fn plex_balance(ex: &(impl Executor + ?Sized), user_id: i64) -> Result<i64, db::Error> {
    Ok(Balance::from_row(&ex.query_one("SELECT LEAST(plex_balance, $2::bigint)::bigint AS plex_balance FROM game.accounts WHERE user_id = $1", &[&user_id, &CLIENT_MAX_PLEX]).await?).plex_balance)
}
