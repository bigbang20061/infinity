//! Persistence of asteroid fields and their rocks; space state is managed by mining.
use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

pub(crate) struct Asteroid { pub id: i64, pub field: i64, pub type_id: i32, pub quantity: i64, pub radius: Option<f32>, pub x: f64, pub y: f64, pub z: f64 }
impl Asteroid { fn from_row(r: &Row) -> Self { Self { id: r.get("asteroid_id"), field: r.get("field_id"), type_id: r.get("type_id"), quantity: r.get("quantity"), radius: r.get("radius"), x: r.get("x"), y: r.get("y"), z: r.get("z") } } }
struct Id { asteroid_id: i64 }
impl Id { fn from_row(r: &Row) -> Self { Self { asteroid_id: r.get("asteroid_id") } } }
struct Depleted { depleted_at: Option<i64> }
impl Depleted { fn from_row(r: &Row) -> Self { Self { depleted_at: r.get("depleted_at") } } }
pub(crate) async fn insert(ex: &(impl Executor + ?Sized), system: i64, field: i64, type_id: i32, quantity: i64, radius: f32, pos: [f64; 3]) -> Result<i64, db::Error> {
 Ok(Id::from_row(&ex.query_one("INSERT INTO game.asteroids (system_id, field_id, type_id, quantity, original_quantity, radius, x, y, z)
                 VALUES ($1, $2, $3, $4, $4, $5, $6, $7, $8) RETURNING asteroid_id", &[&system, &field, &type_id, &quantity, &radius, &pos[0], &pos[1], &pos[2]]).await?).asteroid_id)
}
pub(crate) async fn load(ex: &(impl Executor + ?Sized), system: i64) -> Result<Vec<Asteroid>, db::Error> { Ok(ex.query("SELECT asteroid_id, field_id, type_id, quantity, radius, x, y, z FROM game.asteroids WHERE system_id = $1", &[&system]).await?.iter().map(Asteroid::from_row).collect()) }
pub(crate) async fn depleted(ex: &(impl Executor + ?Sized), field: i64) -> Result<Option<Option<i64>>, db::Error> { Ok(ex.query_opt("SELECT depleted_at FROM game.asteroid_fields WHERE field_id = $1", &[&field]).await?.as_ref().map(|r| Depleted::from_row(r).depleted_at)) }
pub(crate) async fn grown(ex: &(impl Executor + ?Sized), field: i64, system: i64, now: i64) -> Result<u64, db::Error> { ex.execute("INSERT INTO game.asteroid_fields (field_id, system_id, grown_at) VALUES ($1, $2, $3)
             ON CONFLICT (field_id) DO UPDATE SET grown_at = EXCLUDED.grown_at, depleted_at = NULL", &[&field, &system, &now]).await }
pub(crate) async fn delete(ex: &(impl Executor + ?Sized), id: i64) -> Result<u64, db::Error> { ex.execute("DELETE FROM game.asteroids WHERE asteroid_id = $1", &[&id]).await }
pub(crate) async fn mark_depleted(ex: &(impl Executor + ?Sized), field: i64, now: i64) -> Result<u64, db::Error> { ex.execute("UPDATE game.asteroid_fields SET depleted_at = $2 WHERE field_id = $1", &[&field, &now]).await }
pub(crate) async fn quantity(ex: &(impl Executor + ?Sized), id: i64, quantity: i64) -> Result<u64, db::Error> { ex.execute("UPDATE game.asteroids SET quantity = $2 WHERE asteroid_id = $1", &[&id, &quantity]).await }
