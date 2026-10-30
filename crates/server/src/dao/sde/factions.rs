//! Startup projection of typed faction data.
//! This server-startup DAO has no direct client mirror.
//! Source: `sde.chr_factions`. Other faction fields and dynamic state are unsupported.

use crate::{db::{executor::Executor, Error}, sde::factions::Faction};

impl Faction {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self { faction_id: row.get("faction_id"), name: row.get("name"), corporation_id: row.get("corporation_id") }
    }
}

pub(crate) async fn load(ex: &(impl Executor + ?Sized)) -> Result<Vec<Faction>, Error> {
    Ok(ex.query("SELECT faction_id::bigint AS faction_id, name, corporation_id::bigint AS corporation_id FROM sde.chr_factions ORDER BY faction_id", &[]).await?.iter().map(Faction::from_row).collect())
}
