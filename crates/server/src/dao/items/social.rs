//! Asset and repair lookups for `charMgr` and `repairSvc`, and positions for
//! the GM `slash` service. Uses `game.items`, `game.characters`,
//! `game.market_orders`, `sde.sta_stations` and `sde.type_attributes`.
//! Wire values, policy, transactions and notifications remain with services.
use crate::db::{executor::Executor, Error};
use tokio_postgres::Row;
struct Worth { worth: f64 }
impl Worth {
    fn from_row(r: &Row) -> Self { Self { worth: r.get("worth") } }
}
pub(crate) async fn worth(ex: &impl Executor, owner: i64) -> Result<f64, Error> {
    let row = ex.query_one(
        "SELECT COALESCE(sum(GREATEST(i.quantity, 1) *\n                    (SELECT min(o.price) FROM game.market_orders o WHERE o.type_id = i.type_id AND NOT o.bid)), 0)::float8 AS worth\n             FROM game.items i WHERE i.owner_id = $1",
        &[&owner],
    ).await?;
    Ok(Worth::from_row(&row).worth)
}
pub(crate) struct Station {
    pub location_id: i64,
    pub count: i64,
    pub type_id: i64,
    pub solar_system_id: i64,
}
impl Station {
    fn from_row(r: &Row) -> Self {
        Self {
            location_id: r.get("location_id"),
            count: r.get("item_count"),
            type_id: r.get("type_id"),
            solar_system_id: r.get("solar_system_id"),
        }
    }
}
pub(crate) async fn stations(ex: &impl Executor, owner: i64) -> Result<Vec<Station>, Error> {
    let rows = ex.query(
        "SELECT i.location_id, count(*) AS item_count, st.type_id::bigint, st.solar_system_id::bigint FROM game.items i\n                 JOIN sde.sta_stations st ON st.station_id = i.location_id\n                 WHERE i.owner_id = $1 GROUP BY i.location_id, st.type_id, st.solar_system_id",
        &[&owner],
    ).await?;
    Ok(rows.iter().map(Station::from_row).collect())
}
pub(crate) struct RepairQuote {
    pub item_id: i64,
    pub type_id: i64,
    pub health: f64,
}
impl RepairQuote {
    fn from_row(r: &Row) -> Self {
        Self { item_id: r.get("item_id"), type_id: r.get("type_id"), health: r.get("health") }
    }
}
pub(crate) async fn repair_quotes(ex: &impl Executor, ids: &[i64], owner: i64, attribute: i32) -> Result<Vec<RepairQuote>, Error> {
    let rows = ex.query(
        "SELECT i.item_id, i.type_id::bigint, COALESCE((SELECT value FROM sde.type_attributes a WHERE a.type_id = i.type_id AND a.attribute_id = $3), 0)::float8 AS health\n                     FROM game.items i WHERE i.item_id = ANY($1) AND i.owner_id = $2",
        &[&ids, &owner, &attribute],
    ).await?;
    Ok(rows.iter().map(RepairQuote::from_row).collect())
}
pub(crate) struct Position {
    pub location_id: i64,
    pub x: f64,
    pub y: f64,
    pub z: f64,
}
impl Position {
    fn from_row(r: &Row) -> Self {
        Self { location_id: r.get("location_id"), x: r.get("x"), y: r.get("y"), z: r.get("z") }
    }
}
pub(crate) async fn position(ex: &impl Executor, id: i64) -> Result<Option<Position>, Error> {
    let row = ex.query_opt(
        "SELECT i.location_id, i.x, i.y, i.z FROM game.items i\n             WHERE i.item_id = COALESCE((SELECT ship_id FROM game.characters WHERE character_id = $1), $1)\n               AND i.x IS NOT NULL",
        &[&id],
    ).await?;
    Ok(row.as_ref().map(Position::from_row))
}
