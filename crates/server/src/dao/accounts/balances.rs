//! PLEX vault balance lookup in `game.accounts` for store and gateway callers.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Unit conversion, transactions, refusals, notifications and World are caller concerns.
use crate::db::{self, executor::Executor};
use tokio_postgres::Row;
struct Balance { plex_balance: i64 }
impl Balance { fn from_row(r: &Row) -> Self { Self { plex_balance: r.get("plex_balance") } } }
pub(crate) async fn plex_balance(ex: &(impl Executor + ?Sized), user_id: i64) -> Result<i64, db::Error> {
    Ok(Balance::from_row(&ex.query_one("SELECT plex_balance FROM game.accounts WHERE user_id = $1", &[&user_id]).await?).plex_balance)
}
