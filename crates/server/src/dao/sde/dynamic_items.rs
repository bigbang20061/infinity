//! Startup projection of native mutaplasmid definitions from `sde_native.dynamicitemattributes`.
//! The client consumes this data for dynamic item mutation; mutation execution is not a DAO concern.
//! Other native dynamic item data is not loaded here.
use crate::db::{executor::Executor, Error};

pub(crate) struct DynamicItemRow {
    pub(crate) key: Option<String>,
    pub(crate) data: String,
}

pub(crate) async fn load(ex: &(impl Executor + ?Sized)) -> Result<Vec<DynamicItemRow>, Error> {
    let rows = ex.query("SELECT key, data::text AS data FROM sde_native.dynamicitemattributes ORDER BY row_no", &[]).await?;
    Ok(rows.iter().map(|row| DynamicItemRow { key: row.get("key"), data: row.get("data") }).collect())
}
