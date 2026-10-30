//! LP store catalogue data for the client loyalty point store.
//! Joins NPC corporation and type SDE data with game retail prices; live offer tables are unavailable.
use crate::db::{executor::Executor, Error};

pub(crate) struct OfferRow { pub type_id: i32, pub average: f64 }
impl OfferRow {
    fn from_row(row: &tokio_postgres::Row) -> Self { Self { type_id: row.get("type_id"), average: row.get("average") } }
}
pub(crate) async fn offers(ex: &impl Executor, corp: i64, meta_group: f64) -> Result<Vec<OfferRow>, Error> {
    Ok(ex.query(
            "SELECT t.type_id, r.average
             FROM sde_native.npccorporations c
             JOIN sde.inv_types t ON t.race_id = (c.data->>'raceID')::int
             JOIN sde.type_attributes a ON a.type_id = t.type_id
             JOIN sde.dogma_attributes d ON d.attribute_id = a.attribute_id AND d.name = 'metaGroupID'
             JOIN game.retail_prices r ON r.type_id = t.type_id AND r.average > 0
             WHERE c.key = $1::bigint::text AND a.value = $2 AND t.published AND t.market_group_id IS NOT NULL
             ORDER BY t.type_id",
            &[&corp, &meta_group],
        ).await?.iter().map(OfferRow::from_row).collect())
}
