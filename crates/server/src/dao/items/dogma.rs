//! Online module state and per-item dogma base overrides in the game database.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Client state, transactions, refusals, notifications and World are caller concerns.
use crate::db::{executor::Executor, Error};
use tokio_postgres::Row;
pub(crate) struct OnlineModule { pub item_id: i64 }
impl OnlineModule { fn from_row(r: &Row) -> Self { Self { item_id: r.get("item_id") } } }
pub(crate) async fn online(ex: &(impl Executor + ?Sized), ship: i64) -> Result<Vec<OnlineModule>, Error> { Ok(ex.query("SELECT item_id FROM game.items WHERE location_id = $1 AND online", &[&ship]).await?.iter().map(OnlineModule::from_row).collect()) }

pub(crate) struct AttributeOverride {
    pub item_id: i64,
    pub attribute_id: i32,
    pub value: f64,
    pub updated_at: i64,
}

impl AttributeOverride {
    fn from_row(r: &Row) -> Self {
        Self { item_id: r.get("item_id"), attribute_id: r.get("attribute_id"), value: r.get("value"), updated_at: r.get("updated_at") }
    }
}

pub(crate) async fn overrides_for(ex: &(impl Executor + ?Sized), ids: &[i64]) -> Result<Vec<AttributeOverride>, Error> {
    Ok(ex.query("SELECT item_id, attribute_id, value, updated_at FROM game.item_attributes WHERE item_id = ANY($1) ORDER BY item_id, attribute_id", &[&ids]).await?.iter().map(AttributeOverride::from_row).collect())
}

pub(crate) async fn overrides(ex: &(impl Executor + ?Sized), item: i64) -> Result<Vec<AttributeOverride>, Error> {
    overrides_for(ex, &[item]).await
}

pub(crate) async fn set_override(ex: &(impl Executor + ?Sized), item: i64, attribute: i32, value: f64) -> Result<bool, Error> {
    Ok(ex.execute("WITH target AS (SELECT item_id FROM game.items WHERE item_id = $1 AND singleton FOR UPDATE) INSERT INTO game.item_attributes (item_id, attribute_id, value) SELECT item_id, $2, $3 FROM target ON CONFLICT (item_id, attribute_id) DO UPDATE SET value = EXCLUDED.value", &[&item, &attribute, &value]).await? != 0)
}

pub(crate) async fn clear_override(ex: &(impl Executor + ?Sized), item: i64, attribute: i32) -> Result<(), Error> {
    ex.execute("DELETE FROM game.item_attributes WHERE item_id = $1 AND attribute_id = $2", &[&item, &attribute]).await?;
    Ok(())
}

pub(crate) async fn clear_overrides(ex: &(impl Executor + ?Sized), item: i64) -> Result<(), Error> {
    ex.execute("DELETE FROM game.item_attributes WHERE item_id = $1", &[&item]).await?;
    Ok(())
}


/// Atomically persist all ship layers in attribute-ID order.
pub(crate) async fn save_condition(ex: &(impl Executor + ?Sized), item: i64, ids: &[i32], values: &[f64], shield: i32, at: i64) -> Result<(), Error> {
    ex.execute("INSERT INTO game.item_attributes (item_id, attribute_id, value, updated_at) SELECT $1, a.id, a.value, CASE WHEN a.id = $4 THEN $5::bigint ELSE 0 END FROM unnest($2::integer[], $3::double precision[]) AS a(id, value) JOIN game.items i ON i.item_id = $1 AND i.singleton ORDER BY a.id ON CONFLICT (item_id, attribute_id) DO UPDATE SET value = EXCLUDED.value, updated_at = EXCLUDED.updated_at", &[&item, &ids, &values, &shield, &at]).await?;
    Ok(())
}
