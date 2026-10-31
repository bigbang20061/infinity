//! killRightMgr client activation and purchase methods.
//! Mirrors eve/client/script/ui/shared/killRights.py; state lives in game.kill_rights.
use super::*;

pub(super) async fn dispatch(c: &Client, call: &Call<'_>) -> Option<CallResult> {
    let id = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
    Some(match call.method {
        "BuyKillRight" => {
            let price = call.args.get(1).and_then(|v| v.as_float().or_else(|| v.as_int().map(|n| n as f64))).unwrap_or(f64::NAN);
            kill_rights::buy(c, id, price).await
        }
        "ActivateKillRight" => kill_rights::activate(c, id).await,
        _ => return None,
    })
}
