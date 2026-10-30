//! Retail prices for the New Eden Store catalogue and LP offers.
//! vgs/client/vgsClient.py uses the reconstructed catalogue; live offer tables are unavailable.
//! Static presentation and eligibility are supplied by the startup SDE dataset.
use crate::db::{executor::Executor, Error};
use std::collections::HashMap;
struct Price { type_id:i32, average:f64 }
impl Price { fn from_row(r:&tokio_postgres::Row)->Self { Self { type_id:r.get("type_id"), average:r.get("average") } } }
/// PostgreSQL orders NaN above all non-NaN values, including zero.
pub(crate) fn positive_price(value: f64) -> bool { value > 0.0 || value.is_nan() }
pub(crate) async fn prices(ex:&(impl Executor + ?Sized))->Result<HashMap<i32,f64>,Error> {
    Ok(ex.query("SELECT type_id, average FROM game.retail_prices", &[]).await?.iter().map(Price::from_row).map(|r| (r.type_id,r.average)).collect())
}
