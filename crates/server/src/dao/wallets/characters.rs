//! Character wallet balance, credit, debit and recipient lookup in `game.characters`.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Transactions, refusals, notifications and World are caller concerns.
//! Exact GM writes use decimal strings; client projections are lossy and clamped.
//! Non-GM overflowing credits remain database errors; no decimal dependency is used.

use crate::consts::CLIENT_MAX_ISK;
use crate::db::{executor::Executor, Error};
use super::Balance;

struct ExactBalance { exact: String, balance: f64 }
impl ExactBalance {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self { exact: row.get("exact"), balance: Balance::from_row(row, CLIENT_MAX_ISK).0 }
    }
}

struct Coverage { covers: bool }
impl Coverage {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self { covers: row.get("covers") }
    }
}

pub(crate) async fn set_exact(ex: &(impl Executor + ?Sized), char_id: i64, amount: &str) -> Result<(String, f64), Error> {
    let row = ex.query_one(
        "UPDATE game.characters SET balance = $2::text::numeric WHERE character_id = $1 RETURNING balance::text AS exact, balance::float8 AS balance",
        &[&char_id, &amount],
    ).await?;
    let balance = ExactBalance::from_row(&row);
    Ok((balance.exact, balance.balance))
}

pub(crate) async fn credit_exact(ex: &(impl Executor + ?Sized), char_id: i64, amount: &str) -> Result<(String, f64), Error> {
    let row = ex.query_one(
        "UPDATE game.characters SET balance = balance + $2::text::numeric WHERE character_id = $1 RETURNING balance::text AS exact, balance::float8 AS balance",
        &[&char_id, &amount],
    ).await?;
    let balance = ExactBalance::from_row(&row);
    Ok((balance.exact, balance.balance))
}

pub(crate) async fn balance(ex: &(impl Executor + ?Sized), char_id: i64) -> Result<f64, Error> {
    Ok(Balance::from_row(&ex.query_one("SELECT balance::float8 FROM game.characters WHERE character_id = $1", &[&char_id]).await?, CLIENT_MAX_ISK).0)
}

pub(crate) async fn covers(ex: &(impl Executor + ?Sized), char_id: i64, amount: f64) -> Result<bool, Error> {
    Ok(Coverage::from_row(&ex.query_one(
        "SELECT balance >= $2::float8::numeric AS covers FROM game.characters WHERE character_id = $1",
        &[&char_id, &amount],
    ).await?).covers)
}

pub(crate) async fn credit(ex: &(impl Executor + ?Sized), char_id: i64, amount: f64) -> Result<f64, Error> {
    Ok(Balance::from_row(&ex.query_one(
        "UPDATE game.characters SET balance = balance + $2::float8::numeric WHERE character_id = $1 RETURNING balance::float8",
        &[&char_id, &amount],
    ).await?, CLIENT_MAX_ISK).0)
}

pub(crate) async fn debit(ex: &(impl Executor + ?Sized), char_id: i64, amount: f64) -> Result<Option<f64>, Error> {
    Ok(ex.query_opt(
        "UPDATE game.characters SET balance = balance - $2::float8::numeric
             WHERE character_id = $1 AND balance >= $2::float8::numeric RETURNING balance::float8",
        &[&char_id, &amount],
    ).await?.map(|row| Balance::from_row(&row, CLIENT_MAX_ISK).0))
}

pub(crate) async fn recipient_exists(ex: &(impl Executor + ?Sized), char_id: i64) -> Result<bool, Error> {
    Ok(ex.query_opt("SELECT 1 FROM game.characters WHERE character_id = $1 AND NOT deleted", &[&char_id]).await?.is_some())
}
