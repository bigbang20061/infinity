//! Market skill level reads for marketProxy (`eve/client/script/ui/services/marketsvc.py`),
//! using `game.skills`. Market policy, transactions, notifications and refusals remain
//! in the service; this module provides data only.

use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

pub(crate) struct SkillLevel { pub type_id: i32, pub level: i32 }
impl SkillLevel {
    fn from_row(row: &Row) -> Self { Self { type_id: row.get("type_id"), level: row.get("level") } }
}

pub(crate) async fn levels(ex: &(impl Executor + ?Sized), char_id: i64, wanted: &[i32]) -> Result<Vec<SkillLevel>, db::Error> {
    Ok(ex.query("SELECT type_id, level FROM game.skills WHERE character_id = $1 AND type_id = ANY($2)", &[&char_id, &wanted]).await?.iter().map(SkillLevel::from_row).collect())
}
