//! Transactional claim of an inventory-backed wreck in game.items.
//! The owning effect supplies the SDE type, transaction and post-commit effects.
//! Loot is moved separately before commit; this statement never deletes children.
use crate::db::{executor::Executor, Error};

/// Deleting the exact positioned root claims it for one transaction only.
pub(crate) async fn claim(ex: &(impl Executor + ?Sized), id: i64, system: i64, type_id: i32) -> Result<bool, Error> {
    Ok(ex.execute("DELETE FROM game.items WHERE item_id = $1 AND location_id = $2 AND type_id = $3 AND singleton AND x IS NOT NULL AND y IS NOT NULL AND z IS NOT NULL", &[&id, &system, &type_id]).await? == 1)
}
