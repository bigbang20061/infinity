//! Ship-related skill level lookups in `game.skills`.
//! This DAO has no client service; policy and effects belong to callers.
use crate::db::{self, executor::Executor};
use tokio_postgres::Row;
struct Level { level: i32 }
impl Level { fn from_row(r: &Row) -> Self { Self { level: r.get("level") } } }
pub(crate) async fn level(ex: &(impl Executor + ?Sized), char_id: i64, skill_id: i32) -> Result<Option<i32>, db::Error> {
    Ok(ex.query_opt("SELECT level FROM game.skills WHERE character_id = $1 AND type_id = $2", &[&char_id, &skill_id]).await?.as_ref().map(Level::from_row).map(|r| r.level))
}
