//! Startup projections of typed industry blueprints and station operations.
//! Consumers mirror `eve/client/script/industry`; unsupported activities and native tables are omitted.

use crate::db::{self, executor::Executor};
use crate::sde::industry::{MANUFACTURING, RESEARCH_TIME, RESEARCH_MATERIAL, COPYING, INVENTION};
use tokio_postgres::Row;

pub(crate) struct Head { pub blueprint: i32, pub activity: i32, pub time: Option<f64>, pub max_production_limit: i32 }
impl Head { fn from_row(r: &Row) -> Self { Self { blueprint: r.get("blueprint"), activity: r.get("activity"), time: r.get("time"), max_production_limit: r.get("max_production_limit") } } }
pub(crate) struct Part { pub blueprint: i32, pub activity: i32, pub part: String, pub type_id: i32, pub quantity: i64, pub level: i32, pub probability: f64 }
impl Part { fn from_row(r: &Row) -> Self { Self { blueprint: r.get("blueprint"), activity: r.get("activity"), part: r.get("part"), type_id: r.get("type_id"), quantity: r.get("quantity"), level: r.get("level"), probability: r.get("probability") } } }
pub(crate) struct Operation { pub operation_id: i64, pub services: Vec<i32> }
impl Operation { fn from_row(r: &Row) -> Self { Self { operation_id: r.get("operation_id"), services: r.get("services") } } }

fn names() -> String {
    format!("(VALUES ({MANUFACTURING}, 'manufacturing'), ({RESEARCH_TIME}, 'research_time'), ({RESEARCH_MATERIAL}, 'research_material'), ({COPYING}, 'copying'), ({INVENTION}, 'invention')) AS a(activity, name)")
}
pub(crate) async fn activities(ex: &(impl Executor + ?Sized)) -> Result<Vec<Head>, db::Error> {
    let sql = format!("SELECT b.blueprint_type_id::int AS blueprint, a.activity, (b.activities->a.name->>'time')::float8 AS time, COALESCE(b.max_production_limit, 1)::int AS max_production_limit FROM sde.blueprints b CROSS JOIN {} WHERE b.activities ? a.name ORDER BY b.blueprint_type_id, a.activity", names());
    Ok(ex.query(&sql, &[]).await?.iter().map(Head::from_row).collect())
}
pub(crate) async fn parts(ex: &(impl Executor + ?Sized)) -> Result<Vec<Part>, db::Error> {
    let sql = format!("SELECT b.blueprint_type_id::int AS blueprint, a.activity, p.part, (e.value->>'typeID')::int AS type_id, COALESCE((e.value->>'quantity')::bigint, 1) AS quantity, COALESCE((e.value->>'level')::int, 0) AS level, COALESCE((e.value->>'probability')::float8, 1) AS probability FROM sde.blueprints b CROSS JOIN {} CROSS JOIN (VALUES ('materials'), ('products'), ('skills')) p(part) CROSS JOIN LATERAL jsonb_array_elements(COALESCE(b.activities->a.name->p.part, '[]'::jsonb)) WITH ORDINALITY e(value, ord) WHERE b.activities ? a.name ORDER BY b.blueprint_type_id, a.activity, p.part, e.ord", names());
    Ok(ex.query(&sql, &[]).await?.iter().map(Part::from_row).collect())
}
pub(crate) async fn operations(ex: &(impl Executor + ?Sized)) -> Result<Vec<Operation>, db::Error> {
    Ok(ex.query("SELECT operation_id::bigint, COALESCE(service_ids::integer[], ARRAY[]::integer[]) AS services FROM sde.sta_operations ORDER BY operation_id", &[]).await?.iter().map(Operation::from_row).collect())
}
