//! Mining and reprocessing item persistence; gameplay and notifications remain in services.
//! Shared stack lookup and mutations live in `items::transfers`.
use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

struct Used { used: f64 }
impl Used { fn from_row(r: &Row) -> Self { Self { used: r.get("used") } } }
pub(crate) struct Material { pub material_type_id: i32, pub quantity: Option<i64>, pub average: f64 }
impl Material { fn from_row(r: &Row) -> Self { Self { material_type_id: r.get("material_type_id"), quantity: r.get("quantity"), average: r.get("average") } } }
pub(crate) async fn used(ex: &(impl Executor + ?Sized), ship: i64, flag: i32) -> Result<f64, db::Error> { Ok(Used::from_row(&ex.query_one("SELECT COALESCE(sum(i.quantity * COALESCE(t.volume, 0)), 0)::float8 AS used FROM game.items i JOIN sde.inv_types t USING (type_id)
             WHERE i.location_id = $1 AND i.flag_id = $2", &[&ship, &flag]).await?).used) }
pub(crate) async fn materials(ex: &(impl Executor + ?Sized), type_id: i32) -> Result<Vec<Material>, db::Error> { Ok(ex.query("SELECT m.material_type_id, m.quantity, COALESCE(p.average, 0)::float8 AS average FROM sde.inv_type_materials m
             LEFT JOIN game.retail_prices p ON p.type_id = m.material_type_id WHERE m.type_id = $1", &[&type_id]).await?.iter().map(Material::from_row).collect()) }
