//! PostgreSQL case folding for slash type lookup; there is no direct client mirror.
//! Uses no game tables; matching and type selection remain in the slash service.
//! Only the input pattern is folded here, not the startup SDE dataset.
use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

struct LowerPattern { value: String }
impl LowerPattern {
    fn from_row(row: &Row) -> Self { Self { value: row.get("value") } }
}

pub(crate) async fn lower_pattern(ex: &(impl Executor + ?Sized), pattern: &str) -> Result<String, db::Error> {
    Ok(LowerPattern::from_row(&ex.query_one("SELECT lower($1::text) AS value", &[&pattern]).await?).value)
}
