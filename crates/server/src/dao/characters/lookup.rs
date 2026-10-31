//! Exact, active character identity lookup for GM commands.
//! The slash service has no corresponding client-side database query.
//! Reads `game.characters`; deleted characters are excluded.
use crate::db::{executor::Executor, Error};
use tokio_postgres::Row;

struct CharacterId { id: i64 }
impl CharacterId { fn from_row(row: &Row) -> Self { Self { id: row.get("character_id") } } }

pub(crate) async fn find(ex: &(impl Executor + ?Sized), text: &str, id: Option<i64>) -> Result<Option<i64>, Error> {
    Ok(ex.query_opt("SELECT character_id FROM game.characters WHERE NOT deleted AND (character_id = $2 OR ($2::bigint IS NULL AND lower(name) = lower($1))) ORDER BY character_id LIMIT 1", &[&text, &id]).await?.as_ref().map(|row| CharacterId::from_row(row).id))
}
