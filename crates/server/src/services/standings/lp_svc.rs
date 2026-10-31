//! LPSvc mirrors `eve/client/script/ui/shared/neocom/wallet/loyaltyPointsWalletSvc.py`.
//! Character and corporation balances use `game.lp_balances`, keyed by owner and issuer;
//! this service lists balances only and does not settle offers or Analysis Kredits.
use crate::services::{db_err, Call, CallResult};
use crate::client::Client;
use marshal::Value;

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let me = c.session.int("charid").unwrap_or(0);
    let owner = match call.method {
        "GetAllMyCharacterWalletLPBalances" => me,
        "GetAllMyCorporationWalletLPBalances" => match super::super::corps::membership(&c.db, me).await {
            Ok((corp, _)) => corp,
            Err(e) => return Some(Err(e)),
        },
        _ => return None,
    };
    Some(match crate::dao::wallets::loyalty::list(&c.db, owner).await {
        Ok(rows) => Ok(Value::list(rows.iter().map(|r| Value::tuple([Value::Int(r.corp_id), Value::Int(r.amount)])))),
        Err(e) => Err(db_err(e)),
    })
}
