//! Active fit data from game items, skills and module charges.
//! This DAO has no direct client service; dogma evaluation remains in the caller.
//! Shared item type lookup lives in `items::inventory`.
use crate::db::{executor::Executor, Error};
use crate::consts::{FLAG_FITTING_FIRST, FLAG_FITTING_LAST, FLAG_RIG_FIRST, FLAG_RIG_LAST, FLAG_SUBSYSTEM_FIRST, FLAG_SUBSYSTEM_LAST};
use tokio_postgres::Row;

pub(crate) struct Module { pub item_id: i64, pub type_id: i32, pub online: bool, pub flag_id: i32 }
impl Module { fn from_row(r: &Row) -> Self { Self { item_id: r.get("item_id"), type_id: r.get("type_id"), online: r.get("online"), flag_id: r.get("flag_id") } } }
pub(crate) struct Charge { pub flag_id: i32, pub type_id: i32 }
impl Charge { fn from_row(r: &Row) -> Self { Self { flag_id: r.get("flag_id"), type_id: r.get("type_id") } } }

pub(crate) async fn modules(ex: &(impl Executor + ?Sized), ship: i64) -> Result<Vec<Module>, Error> {
    Ok(ex.query("SELECT item_id, type_id, COALESCE(online, false) AS online, flag_id FROM game.items WHERE location_id = $1
                 AND (flag_id BETWEEN $2 AND $3 OR flag_id BETWEEN $4 AND $5 OR flag_id BETWEEN $6 AND $7)",
        &[&ship, &FLAG_FITTING_FIRST, &FLAG_FITTING_LAST, &FLAG_RIG_FIRST, &FLAG_RIG_LAST, &FLAG_SUBSYSTEM_FIRST, &FLAG_SUBSYSTEM_LAST]).await?.iter().map(Module::from_row).collect())
}
pub(crate) async fn charges(ex: &(impl Executor + ?Sized), ship: i64) -> Result<Vec<Charge>, Error> {
    Ok(ex.query("SELECT flag_id, type_id FROM game.module_charges WHERE ship_id = $1", &[&ship]).await?.iter().map(Charge::from_row).collect())
}
