//! LPSvc answers loyalty wallet queries using game.lp_balances; corporation wallets are not modelled.
use crate::services::{db_err, Call, CallResult};
use crate::client::Client;
use marshal::Value;

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let me = c.session.int("charid").unwrap_or(0);
    Some(match call.method {
        "GetAllMyCharacterWalletLPBalances" => match crate::dao::wallets::loyalty::list(&c.db, me).await {
            Ok(rows) => Ok(Value::list(rows.iter().map(|r| Value::tuple([Value::Int(r.corp_id), Value::Int(r.amount)])))),
            Err(e) => Err(db_err(e)),
        },
        "GetAllMyCorporationWalletLPBalances" => Ok(Value::list([])),
        _ => return None,
    })
}
