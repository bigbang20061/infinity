//! Clone capsule deletion in game.items; jumpCloneSvc owns policy and notifications.
use crate::db::{self, executor::Executor};
pub(crate) async fn delete_capsule(ex: &(impl Executor + ?Sized), ship: i64, capsule_type: i32) -> Result<u64, db::Error> {
    ex.execute("DELETE FROM game.items WHERE item_id = $1 AND type_id = $2", &[&ship, &capsule_type]).await
}
