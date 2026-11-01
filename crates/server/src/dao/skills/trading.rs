//! Guarded skill economy writes to game.skills.
//! Client policy, serialization and notifications belong to the caller.
use crate::db::{executor::Executor, Error};

pub(crate) async fn replace_skill(ex: &(impl Executor + ?Sized), character: i64, type_id: i32, old_level: i32, old_points: i64, level: i32, points: i64) -> Result<bool, Error> {
    Ok(ex.execute("UPDATE game.skills SET level = $5, skill_points = $6 WHERE character_id = $1 AND type_id = $2 AND level = $3 AND skill_points = $4", &[&character, &type_id, &old_level, &old_points, &level, &points]).await? == 1)
}

pub(crate) async fn learn(ex: &(impl Executor + ?Sized), character: i64, type_id: i32) -> Result<bool, Error> {
    Ok(ex.execute("INSERT INTO game.skills (character_id, type_id, level, skill_points) VALUES ($1, $2, 0, 0) ON CONFLICT (character_id, type_id) DO NOTHING", &[&character, &type_id]).await? == 1)
}
