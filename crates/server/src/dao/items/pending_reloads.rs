//! Persistence of in-flight reload reservations in `game.pending_reloads`.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Reload policy, transactions, recovery, notifications and World are caller concerns.
use crate::db::{executor::Executor, Error};
use tokio_postgres::Row;

pub(crate) struct PendingReload {
    pub reload_id: i64,
    pub owner_id: i64,
    pub ship_id: i64,
    pub module_id: i64,
    pub flag_id: i32,
    pub type_id: i32,
    pub quantity: i32,
}

impl PendingReload {
    fn from_row(row: &Row) -> Self {
        Self {
            reload_id: row.get("reload_id"),
            owner_id: row.get("owner_id"),
            ship_id: row.get("ship_id"),
            module_id: row.get("module_id"),
            flag_id: row.get("flag_id"),
            type_id: row.get("type_id"),
            quantity: row.get("quantity"),
        }
    }
}

struct ReloadId {
    reload_id: i64,
}

impl ReloadId {
    fn from_row(row: &Row) -> Self {
        Self { reload_id: row.get("reload_id") }
    }
}

pub(crate) async fn insert(
    ex: &(impl Executor + ?Sized),
    owner: i64,
    ship: i64,
    module: i64,
    flag: i32,
    type_id: i32,
    quantity: i32,
) -> Result<i64, Error> {
    Ok(ReloadId::from_row(&ex.query_one(
        "INSERT INTO game.pending_reloads (owner_id, ship_id, module_id, flag_id, type_id, quantity)
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING reload_id",
        &[&owner, &ship, &module, &flag, &type_id, &quantity],
    ).await?).reload_id)
}

pub(crate) async fn all(ex: &(impl Executor + ?Sized)) -> Result<Vec<PendingReload>, Error> {
    Ok(ex.query(
        "SELECT reload_id, owner_id, ship_id, module_id, flag_id, type_id, quantity
         FROM game.pending_reloads ORDER BY reload_id",
        &[],
    ).await?.iter().map(PendingReload::from_row).collect())
}

pub(crate) async fn for_flags(ex: &(impl Executor + ?Sized), ship: i64, flags: &[i32]) -> Result<Vec<PendingReload>, Error> {
    Ok(ex.query(
        "SELECT reload_id, owner_id, ship_id, module_id, flag_id, type_id, quantity
         FROM game.pending_reloads WHERE ship_id = $1 AND flag_id = ANY($2) ORDER BY reload_id",
        &[&ship, &flags],
    ).await?.iter().map(PendingReload::from_row).collect())
}

pub(crate) async fn take(ex: &(impl Executor + ?Sized), reload_id: i64) -> Result<Option<PendingReload>, Error> {
    Ok(ex.query_opt(
        "DELETE FROM game.pending_reloads WHERE reload_id = $1
         RETURNING reload_id, owner_id, ship_id, module_id, flag_id, type_id, quantity",
        &[&reload_id],
    ).await?.as_ref().map(PendingReload::from_row))
}
