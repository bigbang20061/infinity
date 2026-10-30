//! Ship position and version persistence for snapshots in `game.items`.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Snapshot generation and acknowledgement, transactions, refusals, notifications and World are caller concerns.
use crate::db::{self, executor::Executor};
use tokio_postgres::Row;
use std::collections::{HashMap, HashSet};
struct Version { item_id: i64, version: String }
impl Version { fn from_row(r: &Row) -> Self { Self { item_id: r.get("item_id"), version: r.get("version") } } }
struct ItemId { item_id: i64 }
impl ItemId { fn from_row(r: &Row) -> Self { Self { item_id: r.get("item_id") } } }
pub(crate) async fn save(ex: &(impl Executor + ?Sized), ship: i64, pos: &[f64; 3]) -> Result<u64, db::Error> {
    ex.execute("UPDATE game.items SET x = $2, y = $3, z = $4 WHERE item_id = $1", &[&ship, &pos[0], &pos[1], &pos[2]]).await
}
pub(crate) async fn versions(ex: &(impl Executor + ?Sized), ids: &[i64], system: i64) -> Result<HashMap<i64, String>, db::Error> {
    Ok(ex.query("SELECT item_id, xmin::text AS version FROM game.items WHERE item_id = ANY($1::bigint[]) AND location_id = $2", &[&ids, &system]).await?.iter().map(Version::from_row).map(|v| (v.item_id, v.version)).collect())
}
pub(crate) async fn save_snapshot(ex: &(impl Executor + ?Sized), ids: &[i64], versions: &[&str], xs: &[f64], ys: &[f64], zs: &[f64], system: i64) -> Result<HashSet<i64>, db::Error> {
    Ok(ex.query("UPDATE game.items AS i SET x = v.x, y = v.y, z = v.z
         FROM unnest($1::bigint[], $2::text[], $3::float8[], $4::float8[], $5::float8[]) AS v(id, version, x, y, z)
         WHERE i.item_id = v.id AND i.location_id = $6 AND i.xmin::text = v.version RETURNING i.item_id", &[&ids, &versions, &xs, &ys, &zs, &system]).await?.iter().map(ItemId::from_row).map(|r| r.item_id).collect())
}
