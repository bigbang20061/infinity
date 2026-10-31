//! Exact, active alliance identity lookup for GM commands.
//! The slash service has no corresponding client-side database query.
//! Reads `game.alliances`; deleted alliances are excluded.
use crate::db::{executor::Executor, Error};
use tokio_postgres::Row;

struct AllianceId { id: i64 }
impl AllianceId { fn from_row(row: &Row) -> Self { Self { id: row.get("alliance_id") } } }

pub(crate) async fn find(ex: &(impl Executor + ?Sized), text: &str, id: Option<i64>) -> Result<Option<i64>, Error> {
    Ok(ex.query_opt("SELECT alliance_id FROM game.alliances WHERE NOT deleted AND (alliance_id = $2 OR ($2::bigint IS NULL AND lower(name) = lower($1))) ORDER BY alliance_id LIMIT 1", &[&text, &id]).await?.as_ref().map(|row| AllianceId::from_row(row).id))
}
