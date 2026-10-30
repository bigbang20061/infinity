//! Shared character and corporation wallet operations, not a direct client module mirror.
//! Uses game.characters and game.corp_wallets through the DAOs.
//! This helper does not keep journals; callers own transactions.

use super::{db_err, user_error, CallError};
use super::corps::is_player_corp;
use crate::consts::ACCOUNT_CASH;
use crate::dao::{characters::members, wallets::{characters, corporations}};
use crate::db::{executor::Executor, outbox::Outbox};
use crate::messages;
use marshal::Value;

pub(super) async fn balance(db: &(impl Executor + ?Sized), char_id: i64) -> Result<f64, CallError> {
    characters::balance(db, char_id).await.map_err(db_err)
}

pub(super) fn not_enough_money(have: f64, amount: f64) -> CallError {
    user_error(messages::MSG_NOT_ENOUGH_MONEY, vec![("balance".into(), Value::Float(have)), ("amount".into(), Value::Float(amount))])
}

pub(super) async fn credit<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, char_id: i64, amount: f64) -> Result<(), CallError> {
    if amount <= 0.0 { return Ok(()); }
    let balance = characters::credit(db, char_id, amount).await.map_err(db_err)?;
    outbox.notify_pilot(char_id, "OnAccountChange", vec![Value::str("cash"), Value::Int(char_id), Value::Float(balance)]);
    Ok(())
}

pub(super) async fn debit<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, char_id: i64, amount: f64) -> Result<(), CallError> {
    if amount <= 0.0 { return Ok(()); }
    match characters::debit(db, char_id, amount).await.map_err(db_err)? {
        Some(balance) => {
            outbox.notify_pilot(char_id, "OnAccountChange", vec![Value::str("cash"), Value::Int(char_id), Value::Float(balance)]);
            Ok(())
        }
        None => Err(not_enough_money(balance(db, char_id).await?, amount)),
    }
}

pub(super) async fn corp_balance(db: &(impl Executor + ?Sized), corp: i64, key: i32) -> Result<f64, CallError> {
    corporations::balance(db, corp, key).await.map_err(db_err)
}

/// The OnAccountChange name of a division: 'cash', 'cash2' ... 'cash7'.
fn key_name(key: i32) -> String {
    match key - ACCOUNT_CASH {
        0 => "cash".to_string(),
        n => format!("cash{}", n + 1),
    }
}

async fn announce<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, corp: i64, key: i32, balance: f64) -> Result<(), CallError> {
    for id in members::active_ids(db, corp).await.map_err(db_err)? {
        outbox.notify_pilot(id, "OnAccountChange", vec![Value::str(key_name(key)), Value::Int(corp), Value::Float(balance)]);
    }
    Ok(())
}

pub(super) async fn corp_credit<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, corp: i64, key: i32, amount: f64) -> Result<(), CallError> {
    if amount <= 0.0 || !is_player_corp(corp) { return Ok(()); }
    let balance = corporations::credit(db, corp, key, amount).await.map_err(db_err)?;
    announce(db, outbox, corp, key, balance).await
}

pub(super) async fn corp_debit<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, corp: i64, key: i32, amount: f64) -> Result<(), CallError> {
    if amount <= 0.0 { return Ok(()); }
    match corporations::debit(db, corp, key, amount).await.map_err(db_err)? {
        Some(balance) => announce(db, outbox, corp, key, balance).await,
        None => Err(CallError::User {
            msg: messages::MSG_NOT_ENOUGH_MONEY.into(),
            dict: vec![("balance".into(), Value::Float(corp_balance(db, corp, key).await?)), ("amount".into(), Value::Float(amount))],
        }),
    }
}
