//! Analyzer item lookup used by hacking; attributes are loaded from the immutable SDE.
//! Client game state and notifications remain in the hacking service.
use crate::db::{self, executor::Executor};

pub(crate) async fn analyzer(ex: &(impl Executor + ?Sized), module: i64, ship: i64) -> Result<Option<i32>, db::Error> {
    Ok(ex.query_opt("SELECT type_id FROM game.items WHERE item_id = $1 AND location_id = $2", &[&module, &ship]).await?.map(|r| r.get("type_id")))
}
