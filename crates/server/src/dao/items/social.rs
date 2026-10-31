//! Asset lookups for `charMgr`, and positions for the GM `slash` service.
//! Uses `game.items`, `game.characters` and `game.market_orders`; services
//! enrich station data from SDE.
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
}
impl Station {
    fn from_row(r: &Row) -> Self {
        Self {
            location_id: r.get("location_id"),
            count: r.get("item_count"),
        }
    }
}
pub(crate) async fn stations(ex: &impl Executor, owner: i64) -> Result<Vec<Station>, Error> {
    let rows = ex.query(
        "SELECT i.location_id, count(*) AS item_count FROM game.items i\n                 WHERE i.owner_id = $1 GROUP BY i.location_id",
        &[&owner],
    ).await?;
    Ok(rows.iter().map(Station::from_row).collect())
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
