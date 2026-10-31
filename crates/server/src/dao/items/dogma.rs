//! Online module state and per-item dogma base overrides in the game database.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Client state, transactions, refusals, notifications and World are caller concerns.
use crate::db::{executor::Executor, Error};
use tokio_postgres::Row;
pub(crate) struct OnlineModule { pub item_id: i64 }
impl OnlineModule { fn from_row(r: &Row) -> Self { Self { item_id: r.get("item_id") } } }
pub(crate) async fn online(ex: &(impl Executor + ?Sized), ship: i64) -> Result<Vec<OnlineModule>, Error> { Ok(ex.query("SELECT item_id FROM game.items WHERE location_id = $1 AND online", &[&ship]).await?.iter().map(OnlineModule::from_row).collect()) }
pub(crate) async fn set_online(ex: &(impl Executor + ?Sized), module: i64, ship: i64, online: bool) -> Result<(), Error> { ex.execute("UPDATE game.items SET online = $3 WHERE item_id = $1 AND location_id = $2", &[&module, &ship, &online]).await?; Ok(()) }

pub(crate) struct AttributeOverride {
    pub item_id: i64,
    pub attribute_id: i32,
    pub value: f64,
}

impl AttributeOverride {
    fn from_row(r: &Row) -> Self {
        Self { item_id: r.get("item_id"), attribute_id: r.get("attribute_id"), value: r.get("value") }
    }
}

pub(crate) async fn overrides_for(ex: &(impl Executor + ?Sized), ids: &[i64]) -> Result<Vec<AttributeOverride>, Error> {
    Ok(ex.query("SELECT item_id, attribute_id, value FROM game.item_attributes WHERE item_id = ANY($1) ORDER BY item_id, attribute_id", &[&ids]).await?.iter().map(AttributeOverride::from_row).collect())
}
