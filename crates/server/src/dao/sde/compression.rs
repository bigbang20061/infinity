//! Startup source/output mappings from `sde_native.compressibletypes`.
//! The client consumes these pairs in `itemcompression/data.py`; ratios are not stored.
use crate::db::{executor::Executor, Error};

pub(crate) struct CompressionRow {
    pub(crate) source: Option<String>,
    pub(crate) output: Option<String>,
}

pub(crate) async fn load(ex: &(impl Executor + ?Sized)) -> Result<Vec<CompressionRow>, Error> {
    let rows = ex.query("SELECT key AS source, data->>'value' AS output FROM sde_native.compressibletypes", &[]).await?;
    Ok(rows.iter().map(|row| CompressionRow { source: row.get("source"), output: row.get("output") }).collect())
}
