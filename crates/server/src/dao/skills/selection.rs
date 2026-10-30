//! Character skill presence lookup in `game.skills` for character selection.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Starting skill grants, transactions, refusals, notifications and World are caller concerns.
use crate::db::{self, executor::Executor};
pub(crate) async fn has_skills(ex: &(impl Executor + ?Sized), character_id: i64) -> Result<bool, db::Error> {
    Ok(ex.query_opt("SELECT 1 FROM game.skills WHERE character_id = $1 LIMIT 1", &[&character_id]).await?.is_some())
}
