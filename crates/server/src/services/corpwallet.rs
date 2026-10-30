//! Corporation wallets (account service): seven divisions per player corporation
//! (accountKey 1000-1006, game.corp_wallets), and ISK transfers between characters and
//! corporations (GiveCash, GiveCashFromCorpAccount).
//!
//! Members see balances; accountants, directors and the CEO, or holders of a division's
//! corpRoleAccountCanTake role, pay out of it. Online members hear of every balance change
//! (OnAccountChange with the division's 'cash'..'cash7' name). Journals aren't kept.

use super::corps::{ceo_of, is_player_corp, membership};
use super::{active_corporation_member_ids, db_err, has_role_or_is_ceo, keyval, notify_error, Call, CallError, CallResult};
use crate::client::Client;
use crate::consts::{ACCOUNT_CASH, ROLE_ACCOUNTANT, ROLE_DIRECTOR};
use crate::db::Db;
use crate::space::World;
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

/// The OnAccountChange name of a division: 'cash', 'cash2' ... 'cash7'.
fn key_name(key: i32) -> String {
    match key - ACCOUNT_CASH {
        0 => "cash".to_string(),
        n => format!("cash{}", n + 1),
    }
}

pub(super) async fn corp_balance(db: &Db, corp: i64, key: i32) -> Result<f64, CallError> {
    Ok(db
        .client
        .query_opt("SELECT balance::float8 FROM game.corp_wallets WHERE corporation_id = $1 AND account_key = $2", &[&corp, &key])
        .await
        .map_err(db_err)?
        .map(|r| r.get(0))
        .unwrap_or(0.0))
}

/// Tell the corporation's online members a division's new balance.
async fn announce(db: &Db, world: &World, corp: i64, key: i32, balance: f64) -> Result<(), CallError> {
    for id in active_corporation_member_ids(db, corp).await? {
        world.notify_pilot(id, "OnAccountChange", vec![Value::str(key_name(key)), Value::Int(corp), Value::Float(balance)]);
    }
    Ok(())
}

pub(super) async fn corp_credit(db: &Db, world: &World, corp: i64, key: i32, amount: f64) -> Result<(), CallError> {
    if amount <= 0.0 || !is_player_corp(corp) {
        return Ok(());
    }
    let balance: f64 = db
        .client
        .query_one(
            "INSERT INTO game.corp_wallets (corporation_id, account_key, balance) VALUES ($1, $2, $3::float8::numeric)
             ON CONFLICT (corporation_id, account_key) DO UPDATE SET balance = game.corp_wallets.balance + $3::float8::numeric
             RETURNING balance::float8",
            &[&corp, &key, &amount],
        )
        .await
        .map_err(db_err)?
        .get(0);
    announce(db, world, corp, key, balance).await
}

pub(super) async fn corp_debit(db: &Db, world: &World, corp: i64, key: i32, amount: f64) -> Result<(), CallError> {
    if amount <= 0.0 {
        return Ok(());
    }
    let row = db
        .client
        .query_opt(
            "UPDATE game.corp_wallets SET balance = balance - $3::float8::numeric
             WHERE corporation_id = $1 AND account_key = $2 AND balance >= $3::float8::numeric RETURNING balance::float8",
            &[&corp, &key, &amount],
        )
        .await
        .map_err(db_err)?;
    match row {
        Some(r) => announce(db, world, corp, key, r.get(0)).await,
        None => Err(CallError::User {
            msg: "NotEnoughMoney".into(),
            dict: vec![("balance".into(), Value::Float(corp_balance(db, corp, key).await?)), ("amount".into(), Value::Float(amount))],
        }),
    }
}

/// Whether `char_id` may pay out of `corp`'s division `key`.
pub(super) async fn may_take(db: &Db, char_id: i64, corp: i64, key: i32) -> Result<bool, CallError> {
    let can_take = ROLE_CAN_TAKE_1 << (key - ACCOUNT_CASH);
    has_role_or_is_ceo(db, char_id, corp, ROLE_DIRECTOR | ROLE_ACCOUNTANT | can_take).await
}

/// Pay `amount` to `to` (a character, or a player corporation's division `to_key`).
async fn pay_to(db: &Db, world: &World, to: i64, to_key: Option<i32>, amount: f64) -> Result<(), CallError> {
    if is_player_corp(to) {
        let key = to_key.filter(|k| valid_key(*k)).unwrap_or(ACCOUNT_CASH);
        return corp_credit(db, world, to, key, amount).await;
    }
    let exists = db.client.query_opt("SELECT 1 FROM game.characters WHERE character_id = $1 AND NOT deleted", &[&to]).await.map_err(db_err)?.is_some();
    if !exists {
        return Err(notify_error("只能向玩家角色或玩家军团转账。"));
    }
    super::market::credit(db, world, to, amount).await
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
                return Some(Err(notify_error("无效的转账。")));
            }
            let _trading = super::market::lock().await;
            if let Err(e) = super::market::debit(db, world, me, amount).await {
                return Some(Err(e));
            }
            match pay_to(db, world, to, key_kwarg(call, "toAccountKey", 3), amount).await {
                Ok(()) => {
                    info!(user = c.login.user_id, "{me} gave {amount:.2} ISK to {to}");
                    Ok(Value::None)
                }
                Err(e) => {
                    // Put it back: the recipient couldn't take it.
                    let _ = super::market::credit(db, world, me, amount).await;
                    Err(e)
                }
            }
        }
        // GiveCashFromCorpAccount(toID, amount, fromAccountKey, toAccountKey=None, reason='')
        "GiveCashFromCorpAccount" => {
            let to = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
            let amount = amount_arg(call, 1);
            let from_key = key_kwarg(call, "fromAccountKey", 2).unwrap_or(ACCOUNT_CASH);
            let to_key = key_kwarg(call, "toAccountKey", 3);
            if amount <= 0.0 || !valid_key(from_key) || (to == corp && to_key == Some(from_key)) {
                return Some(Err(notify_error("无效的转账。")));
            }
            match may_take(db, me, corp, from_key).await {
                Ok(true) => {}
                Ok(false) => return Some(Err(notify_error("你没有从该部门钱包取款的职权。"))),
                Err(e) => return Some(Err(e)),
            }
            let _trading = super::market::lock().await;
            if let Err(e) = corp_debit(db, world, corp, from_key, amount).await {
                return Some(Err(e));
            }
            match pay_to(db, world, to, to_key, amount).await {
                Ok(()) => {
                    info!(user = c.login.user_id, "{me} paid {amount:.2} ISK from {corp}/{from_key} to {to}");
                    Ok(Value::None)
                }
                Err(e) => {
                    let _ = corp_credit(db, world, corp, from_key, amount).await;
                    Err(e)
                }
            }
        }
        _ => return None,
    })
}
