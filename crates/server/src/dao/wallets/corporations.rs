//! Corporation division wallet balances, credits and debits in `game.corp_wallets`.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Permissions, transactions, refusals, notifications and World are caller concerns.

use crate::consts::CLIENT_MAX_CORP_ISK;
use crate::db::{executor::Executor, Error};
use super::Balance;

pub(crate) async fn balance(ex: &(impl Executor + ?Sized), corp: i64, key: i32) -> Result<f64, Error> {
    Ok(ex.query_opt("SELECT balance::float8 FROM game.corp_wallets WHERE corporation_id = $1 AND account_key = $2", &[&corp, &key])
        .await?.map(|row| Balance::from_row(&row, CLIENT_MAX_CORP_ISK).0).unwrap_or(0.0))
}

pub(crate) async fn credit(ex: &(impl Executor + ?Sized), corp: i64, key: i32, amount: f64) -> Result<f64, Error> {
    Ok(Balance::from_row(&ex.query_one(
        "INSERT INTO game.corp_wallets (corporation_id, account_key, balance) VALUES ($1, $2, $3::float8::numeric)
             ON CONFLICT (corporation_id, account_key) DO UPDATE SET balance = game.corp_wallets.balance + $3::float8::numeric
             RETURNING balance::float8",
        &[&corp, &key, &amount],
    ).await?, CLIENT_MAX_CORP_ISK).0)
}

pub(crate) async fn debit(ex: &(impl Executor + ?Sized), corp: i64, key: i32, amount: f64) -> Result<Option<f64>, Error> {
    Ok(ex.query_opt(
        "UPDATE game.corp_wallets SET balance = balance - $3::float8::numeric
             WHERE corporation_id = $1 AND account_key = $2 AND balance >= $3::float8::numeric RETURNING balance::float8",
        &[&corp, &key, &amount],
    ).await?.map(|row| Balance::from_row(&row, CLIENT_MAX_CORP_ISK).0))
}
