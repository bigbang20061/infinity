//! Corporation wallets (account service): seven divisions per player corporation
//! (accountKey 1000-1006, game.corp_wallets), and ISK transfers between characters and
//! corporations (GiveCash, GiveCashFromCorpAccount).
//!
//! Members see balances; accountants, directors and the CEO, or holders of a division's
//! corpRoleAccountCanTake role, pay out of it. Online members hear of every balance change
//! (OnAccountChange with the division's 'cash'..'cash7' name). Journals aren't kept.

use super::corps::{ceo_of, is_player_corp, membership};
use super::{db_err, has_role_or_is_ceo, keyval, notify_error, Call, CallError, CallResult};
use super::wallets::{corp_balance, corp_credit, corp_debit};
use crate::dao::wallets::characters;
use crate::client::Client;
use crate::consts::{ACCOUNT_CASH, ROLE_ACCOUNTANT, ROLE_DIRECTOR};
use crate::db::Db;
use crate::db::executor::Executor;
use crate::db::outbox::{Outbox, dispatch_world};
use crate::messages;
use marshal::Value;
use tracing::info;

/// Client corporation wallet convention: seven account divisions.
const DIVISIONS: i32 = 7;
/// Client appConst.corpRoleJuniorAccountant.
const ROLE_JUNIOR_ACCOUNTANT: i64 = 4_503_599_627_370_496;
/// Client appConst.corpRoleAccountCanTake1; other divisions' roles are the next bits.
const ROLE_CAN_TAKE_1: i64 = 134_217_728;

fn valid_key(key: i32) -> bool {
    (ACCOUNT_CASH..ACCOUNT_CASH + DIVISIONS).contains(&key)
}


/// Whether `char_id` may pay out of `corp`'s division `key`.
pub(super) async fn may_take(db: &Db, char_id: i64, corp: i64, key: i32) -> Result<bool, CallError> {
    let can_take = ROLE_CAN_TAKE_1 << (key - ACCOUNT_CASH);
    has_role_or_is_ceo(db, char_id, corp, ROLE_DIRECTOR | ROLE_ACCOUNTANT | can_take).await
}

/// Pay `amount` to `to` (a character, or a player corporation's division `to_key`).
async fn pay_to<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, to: i64, to_key: Option<i32>, amount: f64) -> Result<(), CallError> {
    if is_player_corp(to) {
        let key = to_key.filter(|k| valid_key(*k)).unwrap_or(ACCOUNT_CASH);
        return corp_credit(db, outbox, to, key, amount).await;
    }
    let exists = characters::recipient_exists(db, to).await.map_err(db_err)?;
    if !exists {
        return Err(notify_error(messages::TRANSFER_RECIPIENT_NOT_PLAYER));
    }
    super::wallets::credit(db, outbox, to, amount).await
}

fn amount_arg(call: &Call<'_>, i: usize) -> f64 {
    call.args.get(i).and_then(|v| v.as_float().or_else(|| v.as_int().map(|i| i as f64))).unwrap_or(0.0)
}

fn key_kwarg(call: &Call<'_>, name: &str, i: usize) -> Option<i32> {
    call.kwargs.get(name).or_else(|| call.args.get(i)).and_then(|v| v.as_int()).map(|k| k as i32)
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service != "account" {
        return None;
    }
    let me = c.session.int("charid").unwrap_or(0);
    let corp = c.session.int("corpid").unwrap_or(0);
    let (db, world) = (&c.db, &*c.world);
    Some(match call.method {
        // GetCashBalance(isCorp, accountKey=...): personal balances are services/character.
        "GetCashBalance" if call.args.first().and_then(|v| v.as_int()).unwrap_or(0) != 0 => {
            let key = key_kwarg(call, "accountKey", 1).or(c.session.int("corpAccountKey").map(|k| k as i32)).unwrap_or(ACCOUNT_CASH);
            corp_balance(db, corp, key).await.map(Value::Float)
        }
        // [KeyVal(key, balance)] of the session corporation's divisions.
        "GetWalletDivisionsInfo" => {
            if !is_player_corp(corp) {
                return Some(Ok(Value::list([])));
            }
            let (_, roles) = match membership(db, me).await {
                Ok(m) => m,
                Err(e) => return Some(Err(e)),
            };
            let readable = roles & (ROLE_DIRECTOR | ROLE_ACCOUNTANT | ROLE_JUNIOR_ACCOUNTANT) != 0 || ceo_of(db, corp).await.ok().flatten() == Some(me);
            let mut out = vec![];
            for key in ACCOUNT_CASH..ACCOUNT_CASH + DIVISIONS {
                let balance = if readable { corp_balance(db, corp, key).await.unwrap_or(0.0) } else { 0.0 };
                out.push(keyval([("key", Value::Int(key as i64)), ("balance", Value::Float(balance))]));
            }
            Ok(Value::List(out))
        }
        // GiveCash(toID, amount, reason, toAccountKey=None): from the character's wallet.
        "GiveCash" => {
            let to = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
            let amount = amount_arg(call, 1);
            if amount <= 0.0 || to == me {
                return Some(Err(notify_error(messages::TRANSFER_INVALID)));
            }
            let to_key = key_kwarg(call, "toAccountKey", 3);
            let _trading = super::market::lock().await;
            let result = db.transaction(
                |tx, outbox: &mut Outbox| Box::pin(async move {
                    super::wallets::debit(tx, outbox, me, amount).await?;
                    pay_to(tx, outbox, to, to_key, amount).await
                }),
                |value, outbox| async move { dispatch_world(value, outbox, world) },
            ).await;
            result.map(|()| {
                info!(user = c.login.user_id, "{me} gave {amount:.2} ISK to {to}");
                Value::None
            })
        }
        // GiveCashFromCorpAccount(toID, amount, fromAccountKey, toAccountKey=None, reason='')
        "GiveCashFromCorpAccount" => {
            let to = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
            let amount = amount_arg(call, 1);
            let from_key = key_kwarg(call, "fromAccountKey", 2).unwrap_or(ACCOUNT_CASH);
            let to_key = key_kwarg(call, "toAccountKey", 3);
            if amount <= 0.0 || !valid_key(from_key) || (to == corp && to_key == Some(from_key)) {
                return Some(Err(notify_error(messages::TRANSFER_INVALID)));
            }
            match may_take(db, me, corp, from_key).await {
                Ok(true) => {}
                Ok(false) => return Some(Err(notify_error(messages::DEPARTMENT_WALLET_WITHDRAW_UNAUTHORIZED))),
                Err(e) => return Some(Err(e)),
            }
            let _trading = super::market::lock().await;
            let result = db.transaction(
                |tx, outbox: &mut Outbox| Box::pin(async move {
                    corp_debit(tx, outbox, corp, from_key, amount).await?;
                    pay_to(tx, outbox, to, to_key, amount).await
                }),
                |value, outbox| async move { dispatch_world(value, outbox, world) },
            ).await;
            result.map(|()| {
                info!(user = c.login.user_id, "{me} paid {amount:.2} ISK from {corp}/{from_key} to {to}");
                Value::None
            })
        }
        _ => return None,
    })
}
