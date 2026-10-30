//! Startup projection of typed NPC corporation data.
//! This server-startup DAO has no direct client mirror.
//! Source: `sde.crp_npc_corporations`, including its English description projection.
//! Dynamic member counts and other corporation fields are unsupported.

use crate::{db::{executor::Executor, Error}, sde::corporations::NpcCorporation};

impl NpcCorporation {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self {
            corporation_id: row.get("corporation_id"), name: row.get("name"), ticker: row.get("ticker"),
            description: row.get("description"), tax_rate: row.get("tax_rate"), ceo_id: row.get("ceo_id"),
            station_id: row.get("station_id"), race_id: row.get("race_id"), faction_id: row.get("faction_id"),
            member_limit: row.get("member_limit"),
        }
    }
}

pub(crate) async fn load(ex: &(impl Executor + ?Sized)) -> Result<Vec<NpcCorporation>, Error> {
    Ok(ex.query("SELECT corporation_id::bigint AS corporation_id, name, ticker, data->'description'->>'en' AS description, tax_rate, ceo_id::bigint AS ceo_id, station_id::bigint AS station_id, race_id::bigint AS race_id, faction_id::bigint AS faction_id, member_limit::bigint AS member_limit FROM sde.crp_npc_corporations ORDER BY corporation_id", &[]).await?.iter().map(NpcCorporation::from_row).collect())
}
