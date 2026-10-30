//! Bloodline attributes and free skill points from `game.characters` and `sde.chr_bloodlines`.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Skill policy, transactions, refusals, notifications and World are caller concerns.
use crate::db::{executor::Executor, Error};
use tokio_postgres::Row;

pub(crate) struct BloodlineAttributes { pub charisma: Option<i32>, pub intelligence: Option<i32>, pub memory: Option<i32>, pub perception: Option<i32>, pub willpower: Option<i32> }
impl BloodlineAttributes {
    fn from_row(r: &Row) -> Self { Self { charisma: r.get("charisma"), intelligence: r.get("intelligence"), memory: r.get("memory"), perception: r.get("perception"), willpower: r.get("willpower") } }
}
pub(crate) async fn bloodline(ex: &(impl Executor + ?Sized), char_id: i64) -> Result<BloodlineAttributes, Error> {
    Ok(BloodlineAttributes::from_row(&ex.query_one("SELECT b.charisma, b.intelligence, b.memory, b.perception, b.willpower\n             FROM game.characters ch JOIN sde.chr_bloodlines b USING (bloodline_id) WHERE ch.character_id = $1", &[&char_id]).await?))
}
struct FreePoints { points: i64 }
impl FreePoints { fn from_row(r: &Row) -> Self { Self { points: r.get("free_skill_points") } } }
pub(crate) async fn free_points(ex: &(impl Executor + ?Sized), char_id: i64) -> Result<i64, Error> {
    Ok(FreePoints::from_row(&ex.query_one("SELECT free_skill_points FROM game.characters WHERE character_id = $1", &[&char_id]).await?).points)
}
