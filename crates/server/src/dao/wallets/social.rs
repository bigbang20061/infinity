//! GM `slash` balance adjustment using `game.characters`.
//! Client notifications, policy and transactions are not handled here.
use crate::db::{executor::Executor, Error};
pub(crate) async fn add(ex: &impl Executor, id: i64, amount: f64) -> Result<f64, Error> {
    let row = ex.query_one(
        "UPDATE game.characters SET balance = balance + $2::float8::numeric WHERE character_id = $1 RETURNING balance::float8",
        &[&id, &amount],
    ).await?;
    Ok(super::Balance::from_row(&row).0)
}
