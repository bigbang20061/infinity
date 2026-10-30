//! Analyzer item attributes and character skill levels used by hacking.
//! Client game state and notifications remain in the hacking service.
use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

pub(crate) struct Analyzer { pub type_id: i32, pub skill: i32, pub range: f64, pub duration: f64, pub coherence: f64, pub strength: f64, pub slots: f64 }
impl Analyzer {
    fn from_row(r: &Row) -> Self { Self { type_id: r.get("type_id"), skill: r.get("skill"), range: r.get("range"), duration: r.get("duration"), coherence: r.get("coherence"), strength: r.get("strength"), slots: r.get("slots") } }
}
pub(crate) async fn analyzer(ex: &(impl Executor + ?Sized), module: i64, ship: i64, attrs: [i32; 6]) -> Result<Option<Analyzer>, db::Error> {
    Ok(ex.query_opt("SELECT i.type_id,
                    COALESCE((SELECT value FROM sde.type_attributes WHERE type_id = i.type_id AND attribute_id = $3), 0)::int AS skill,
                    COALESCE((SELECT value FROM sde.type_attributes WHERE type_id = i.type_id AND attribute_id = $4), 6000) AS range,
                    COALESCE((SELECT value FROM sde.type_attributes WHERE type_id = i.type_id AND attribute_id = $5), 5000) AS duration,
                    COALESCE((SELECT value FROM sde.type_attributes WHERE type_id = i.type_id AND attribute_id = $6), 40) AS coherence,
                    COALESCE((SELECT value FROM sde.type_attributes WHERE type_id = i.type_id AND attribute_id = $7), 20) AS strength,
                    COALESCE((SELECT value FROM sde.type_attributes WHERE type_id = i.type_id AND attribute_id = $8), 3) AS slots
             FROM game.items i WHERE i.item_id = $1 AND i.location_id = $2", &[&module, &ship, &attrs[0], &attrs[1], &attrs[2], &attrs[3], &attrs[4], &attrs[5]]).await?.as_ref().map(Analyzer::from_row))
}
