//! Corporation militia enlistment whitelist persistence in `game.corp_enlistment_factions`.
//! This DAO has no direct client mirror or wire contract; the owning service handles those.
//! Validation, transactions and notifications are not handled here.

use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

struct Faction {
    faction_id: i64,
}

impl Faction {
    fn from_row(r: &Row) -> Self {
        Self { faction_id: r.get("faction_id") }
    }
}

pub(crate) async fn allowed(ex: &(impl Executor + ?Sized), corp_id: i64) -> Result<Vec<i64>, db::Error> {
    let rows = ex.query(
        "SELECT faction_id FROM game.corp_enlistment_factions WHERE corporation_id = $1 ORDER BY faction_id",
        &[&corp_id],
    ).await?;
    Ok(rows.iter().map(|r| Faction::from_row(r).faction_id).collect())
}

pub(crate) async fn clear(ex: &(impl Executor + ?Sized), corp_id: i64) -> Result<u64, db::Error> {
    ex.execute("DELETE FROM game.corp_enlistment_factions WHERE corporation_id = $1", &[&corp_id]).await
}

pub(crate) async fn insert(ex: &(impl Executor + ?Sized), corp_id: i64, faction_id: i64) -> Result<u64, db::Error> {
    ex.execute(
        "INSERT INTO game.corp_enlistment_factions (corporation_id, faction_id) VALUES ($1, $2)",
        &[&corp_id, &faction_id],
    ).await
}
