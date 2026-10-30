//! Skill records and training writes using `game.skills`.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Training policy, transactions, refusals, notifications and World are caller concerns.
use crate::db::{executor::Executor, Error};
use std::collections::BTreeMap;
use tokio_postgres::Row;

pub(crate) struct Skill { pub type_id: i32, pub level: i32, pub skill_points: i64 }
impl Skill { fn from_row(r: &Row) -> Self { Self { type_id: r.get("type_id"), level: r.get("level"), skill_points: r.get("skill_points") } } }
struct Total { points: i64 }
impl Total { fn from_row(r: &Row) -> Self { Self { points: r.get("points") } } }
pub(crate) async fn starting_skill(ex: &(impl Executor + ?Sized), char_id: i64, type_id: i32, level: i32, sp: i64) -> Result<(), Error> {
    ex.execute("INSERT INTO game.skills (character_id, type_id, level, skill_points) VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING", &[&char_id, &type_id, &level, &sp]).await?; Ok(())
}
pub(crate) async fn current(ex: &(impl Executor + ?Sized), char_id: i64) -> Result<Vec<Skill>, Error> {
    Ok(ex.query("SELECT type_id, level, skill_points FROM game.skills WHERE character_id = $1", &[&char_id]).await?.iter().map(Skill::from_row).collect())
}
pub(crate) async fn total(ex: &(impl Executor + ?Sized), char_id: i64) -> Result<i64, Error> {
    Ok(Total::from_row(&ex.query_one("SELECT COALESCE(sum(skill_points), 0)::bigint AS points FROM game.skills WHERE character_id = $1", &[&char_id]).await?).points)
}
pub(crate) async fn write_skills(ex: &(impl Executor + ?Sized), char_id: i64, plan: &BTreeMap<i32, (Option<i32>, i64)>) -> Result<(), Error> {
    for (&type_id, &(level, sp)) in plan {
        if let Some(level) = level {
            ex.execute("INSERT INTO game.skills (character_id, type_id, level, skill_points) VALUES ($1, $2, $3, $4)\n                 ON CONFLICT (character_id, type_id) DO UPDATE SET level = GREATEST(game.skills.level, EXCLUDED.level),\n                     skill_points = GREATEST(game.skills.skill_points, EXCLUDED.skill_points)", &[&char_id, &type_id, &level, &sp]).await?;
        } else {
            ex.execute("UPDATE game.skills SET skill_points = GREATEST(skill_points, $3) WHERE character_id = $1 AND type_id = $2", &[&char_id, &type_id, &sp]).await?;
        }
    }
    Ok(())
}
