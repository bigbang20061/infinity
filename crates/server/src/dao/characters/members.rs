//! Active corporation member IDs from `game.characters`, excluding deleted characters.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Membership policy, transactions, refusals, notifications and World are caller concerns.

use crate::db::{executor::Executor, Error};

struct Member { character_id: i64 }

impl Member {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self { character_id: row.get("character_id") }
    }
}

pub(crate) async fn active_ids(ex: &(impl Executor + ?Sized), corp_id: i64) -> Result<Vec<i64>, Error> {
    Ok(ex.query("SELECT character_id FROM game.characters WHERE corporation_id = $1 AND NOT deleted", &[&corp_id])
        .await?.iter().map(|row| Member::from_row(row).character_id).collect())
}
