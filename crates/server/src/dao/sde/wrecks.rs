//! Startup projection of native wreck group and race mappings.
//! No direct client mirror; combat uses `sde_native.wrecks`.
//! Other native wreck fields are not loaded.
use crate::db::{executor::Executor, Error};

pub(crate) struct WreckRow { pub(crate) group_key: String, pub(crate) race_key: Option<String>, pub(crate) type_id: Option<String> }
impl WreckRow {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self { group_key: row.get("group_key"), race_key: row.get("race_key"), type_id: row.get("type_id") }
    }
}
pub(crate) async fn load(ex: &(impl Executor + ?Sized)) -> Result<Vec<WreckRow>, Error> {
    Ok(ex.query("SELECT w.key AS group_key, r.key AS race_key, r.value AS type_id FROM sde_native.wrecks w LEFT JOIN LATERAL jsonb_each_text(CASE WHEN jsonb_typeof(w.data->'raceIdToWreckTypeId') = 'object' THEN w.data->'raceIdToWreckTypeId' ELSE '{}'::jsonb END) r ON true", &[]).await?.iter().map(WreckRow::from_row).collect())
}
