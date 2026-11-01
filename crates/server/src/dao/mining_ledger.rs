//! Immutable personal mining delivery facts in `game.character_mining_ledger`.
//! The personal ledger client is in `eve/client/script/ui/shared/ledger/`.
//! Rows are append-only except for server retention deletion; aggregates and
//! corporate observers are not supported by this DAO.

use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Log {
    pub log_id: i64,
    pub character_id: i64,
    pub event_time: i64,
    pub solar_system_id: i64,
    pub type_id: i32,
    pub quantity: i64,
    pub quantity_wasted: i64,
}

impl Log {
    fn from_row(row: &Row) -> Self {
        Self {
            log_id: row.get("log_id"),
            character_id: row.get("character_id"),
            event_time: row.get("event_time"),
            solar_system_id: row.get("solar_system_id"),
            type_id: row.get("type_id"),
            quantity: row.get("quantity"),
            quantity_wasted: row.get("quantity_wasted"),
        }
    }
}

pub(crate) async fn append(
    ex: &(impl Executor + ?Sized),
    character_id: i64,
    solar_system_id: i64,
    type_id: i32,
    quantity: i64,
    quantity_wasted: i64,
    event_time: i64,
) -> Result<(), db::Error> {
    ex.execute(
        "INSERT INTO game.character_mining_ledger (character_id, solar_system_id, type_id, quantity, quantity_wasted, event_time) VALUES ($1, $2, $3, $4, $5, $6)",
        &[&character_id, &solar_system_id, &type_id, &quantity, &quantity_wasted, &event_time],
    ).await?;
    Ok(())
}

pub(crate) async fn list(
    ex: &(impl Executor + ?Sized),
    character_id: i64,
    since: i64,
    through: i64,
) -> Result<Vec<Log>, db::Error> {
    Ok(ex.query(
        "SELECT log_id, character_id, event_time, solar_system_id, type_id, quantity, quantity_wasted FROM game.character_mining_ledger WHERE character_id = $1 AND event_time >= $2 AND event_time <= $3 ORDER BY event_time DESC, log_id DESC",
        &[&character_id, &since, &through],
    ).await?.iter().map(Log::from_row).collect())
}

pub(crate) async fn delete_before(ex: &(impl Executor + ?Sized), cutoff: i64) -> Result<u64, db::Error> {
    ex.execute("DELETE FROM game.character_mining_ledger WHERE event_time < $1", &[&cutoff]).await
}
