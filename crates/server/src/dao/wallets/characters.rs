//! Character wallet balance, credit, debit and recipient lookup in `game.characters`.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Transactions, refusals, notifications and World are caller concerns.

use crate::db::{executor::Executor, Error};
use super::Balance;

pub(crate) async fn balance(ex: &(impl Executor + ?Sized), char_id: i64) -> Result<f64, Error> {
    Ok(Balance::from_row(&ex.query_one("SELECT balance::float8 FROM game.characters WHERE character_id = $1", &[&char_id]).await?).0)
}

pub(crate) async fn credit(ex: &(impl Executor + ?Sized), char_id: i64, amount: f64) -> Result<f64, Error> {
    Ok(Balance::from_row(&ex.query_one(
        "UPDATE game.characters SET balance = balance + $2::float8::numeric WHERE character_id = $1 RETURNING balance::float8",
        &[&char_id, &amount],
    ).await?).0)
}

pub(crate) async fn debit(ex: &(impl Executor + ?Sized), char_id: i64, amount: f64) -> Result<Option<f64>, Error> {
    Ok(ex.query_opt(
        "UPDATE game.characters SET balance = balance - $2::float8::numeric
             WHERE character_id = $1 AND balance >= $2::float8::numeric RETURNING balance::float8",
        &[&char_id, &amount],
    ).await?.map(|row| Balance::from_row(&row).0))
}

pub(crate) async fn recipient_exists(ex: &(impl Executor + ?Sized), char_id: i64) -> Result<bool, Error> {
    Ok(ex.query_opt("SELECT 1 FROM game.characters WHERE character_id = $1 AND NOT deleted", &[&char_id]).await?.is_some())
}
