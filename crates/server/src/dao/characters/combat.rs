//! Combat bounty persistence in `game.characters` and `game.corporations`.
//! Client rewards, tax policy and notifications remain with combat services.
//! Character balance credits live in `wallets::characters`.
use crate::db::{executor::Executor, Error};
use tokio_postgres::Row;

pub(crate) struct CorporationTax { pub corporation_id: i64, pub tax_rate: f64 }
impl CorporationTax {
    fn from_row(row: &Row) -> Self { Self { corporation_id: row.get("corporation_id"), tax_rate: row.get("tax_rate") } }
}
pub(crate) async fn corporation_tax(ex: &(impl Executor + ?Sized), pilot: i64) -> Result<Option<CorporationTax>, Error> {
    Ok(ex.query_opt("SELECT co.corporation_id, co.tax_rate FROM game.characters ch JOIN game.corporations co ON co.corporation_id = ch.corporation_id
                 WHERE ch.character_id = $1 AND NOT co.deleted", &[&pilot]).await?.as_ref().map(CorporationTax::from_row))
}
