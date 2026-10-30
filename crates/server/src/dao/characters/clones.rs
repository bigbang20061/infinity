//! Jump clone persistence in game tables.
//! Client behavior is owned by jumpCloneSvc; transactions, policy and notifications remain with callers.
use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

pub(crate) struct CloneRow { pub clone_id: i64, pub location_id: i64, pub name: String }
impl CloneRow { fn from_row(r: &Row) -> Self { Self { clone_id: r.get("clone_id"), location_id: r.get("location_id"), name: r.get("name") } } }
pub(crate) async fn clones(ex: &(impl Executor + ?Sized), me: i64) -> Result<Vec<CloneRow>, db::Error> {
    Ok(ex.query("SELECT clone_id, location_id, name FROM game.jump_clones WHERE owner_id = $1 ORDER BY clone_id", &[&me]).await?.iter().map(CloneRow::from_row).collect())
}
struct Location { location_id: i64 }
impl Location { fn from_row(r: &Row) -> Self { Self { location_id: r.get("location_id") } } }
pub(crate) async fn locations(ex: &(impl Executor + ?Sized), me: i64) -> Result<Vec<i64>, db::Error> {
    Ok(ex.query("SELECT location_id FROM game.jump_clones WHERE owner_id = $1", &[&me]).await?.iter().map(|r| Location::from_row(r).location_id).collect())
}
struct LastJump { last_clone_jump: Option<i64> }
impl LastJump { fn from_row(r: &Row) -> Self { Self { last_clone_jump: r.get("last_clone_jump") } } }
pub(crate) async fn last_jump(ex: &(impl Executor + ?Sized), me: i64) -> Result<Option<i64>, db::Error> {
    Ok(LastJump::from_row(&ex.query_one("SELECT last_clone_jump FROM game.characters WHERE character_id = $1", &[&me]).await?).last_clone_jump)
}
pub(crate) async fn target(ex: &(impl Executor + ?Sized), clone_id: i64, me: i64, dest: i64) -> Result<Option<i64>, db::Error> {
    Ok(ex.query_opt("SELECT location_id FROM game.jump_clones WHERE clone_id = $1 AND owner_id = $2 AND location_id = $3", &[&clone_id, &me, &dest]).await?.as_ref().map(|r| Location::from_row(r).location_id))
}
pub(crate) async fn install(ex: &(impl Executor + ?Sized), me: i64, station: i64) -> Result<u64, db::Error> {
    ex.execute("INSERT INTO game.jump_clones (owner_id, location_id) VALUES ($1, $2) ON CONFLICT (owner_id, location_id) DO NOTHING", &[&me, &station]).await
}
pub(crate) async fn delete(ex: &(impl Executor + ?Sized), clone_id: i64) -> Result<u64, db::Error> {
    ex.execute("DELETE FROM game.jump_clones WHERE clone_id = $1", &[&clone_id]).await
}
pub(crate) async fn destroy(ex: &(impl Executor + ?Sized), clone_id: i64, me: i64) -> Result<u64, db::Error> {
    ex.execute("DELETE FROM game.jump_clones WHERE clone_id = $1 AND owner_id = $2", &[&clone_id, &me]).await
}
pub(crate) async fn rename(ex: &(impl Executor + ?Sized), clone_id: i64, me: i64, name: &str) -> Result<u64, db::Error> {
    ex.execute("UPDATE game.jump_clones SET name = $3 WHERE clone_id = $1 AND owner_id = $2", &[&clone_id, &me, &name]).await
}
pub(crate) async fn jump(ex: &(impl Executor + ?Sized), me: i64, capsule: i64, station: i64, system: i64, constellation: i64, region: i64, now: i64) -> Result<u64, db::Error> {
    ex.execute("UPDATE game.characters SET ship_id = $2, station_id = $3, solar_system_id = $4, constellation_id = $5, region_id = $6, last_clone_jump = $7 WHERE character_id = $1", &[&me, &capsule, &station, &system, &constellation, &region, &now]).await
}
