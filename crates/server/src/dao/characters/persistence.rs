//! Active character names for an account from `game.characters`, ordered by character ID.
//! This DAO has no direct client mirror or wire contract; the gateway supplies its response contract.
//! Transactions, refusals, notifications and World are caller concerns.
use crate::db::{self, executor::Executor};
use tokio_postgres::Row;
pub(crate) struct CharacterName { pub character_id: i64, pub name: String }
impl CharacterName { fn from_row(r: &Row) -> Self { Self { character_id: r.get("character_id"), name: r.get("name") } } }
pub(crate) async fn names(ex: &(impl Executor + ?Sized), user: i64) -> Result<Vec<CharacterName>, db::Error> {
    Ok(ex.query("SELECT character_id, name FROM game.characters WHERE user_id = $1 AND NOT deleted ORDER BY character_id", &[&user]).await?.iter().map(CharacterName::from_row).collect())
}
