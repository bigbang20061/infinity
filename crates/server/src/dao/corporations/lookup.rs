//! Exact, active corporation identity lookup for GM commands.
//! The slash service has no corresponding client-side database query.
//! Reads `game.corporations`; deleted corporations are excluded.
use crate::db::{executor::Executor, Error};
use tokio_postgres::Row;

struct CorporationId { id: i64 }
impl CorporationId { fn from_row(row: &Row) -> Self { Self { id: row.get("corporation_id") } } }

pub(crate) async fn find(ex: &(impl Executor + ?Sized), text: &str, id: Option<i64>) -> Result<Option<i64>, Error> {
    Ok(ex.query_opt("SELECT corporation_id FROM game.corporations WHERE NOT deleted AND (corporation_id = $2 OR ($2::bigint IS NULL AND lower(name) = lower($1))) ORDER BY corporation_id LIMIT 1", &[&text, &id]).await?.as_ref().map(|row| CorporationId::from_row(row).id))
}
