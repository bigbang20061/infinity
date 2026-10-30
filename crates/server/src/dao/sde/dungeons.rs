//! Startup projections of dungeon metadata for exploration and missions.
//! The client consumes generated sites via `eve/client/script/parklife/scanSvc.py:253-260`.
//! Source is `sde_native.dungeons`; dungeon layouts and whole JSON objects are not loaded.

use crate::{db::{self, executor::Executor}, sde::dungeons::Dungeon};
use tokio_postgres::Row;

impl Dungeon {
    fn from_row(row: &Row) -> Self {
        Self { id: row.get("id"), name_id: row.get("name_id"), has_name: row.get("has_name"),
            difficulty: row.get("difficulty"), faction_id: row.get("faction_id"),
            archetype: row.get("archetype"), gated: row.get("gated") }
    }
}
pub(crate) async fn load(ex: &(impl Executor + ?Sized)) -> Result<Vec<Dungeon>, db::Error> {
    Ok(ex.query("SELECT key::bigint AS id, (data->>'dungeonNameID')::bigint AS name_id,
        data ? 'dungeonNameID' AS has_name, (data->>'difficulty')::bigint AS difficulty,
        (data->>'factionID')::bigint AS faction_id, data->>'archetypeID' AS archetype,
        COALESCE(data->'connections' @? '$[*].allowedShipsList', false) AS gated
        FROM sde_native.dungeons ORDER BY key::bigint", &[]).await?.iter().map(Dungeon::from_row).collect())
}
