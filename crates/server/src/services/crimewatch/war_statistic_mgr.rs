//! Individual kill reports for `warStatisticMgr`.
//! Answers the client's `GetKillMail` call using `game.killmails`.
//! Alliance, faction and war fields not stored by the server are returned empty.

use super::*;

pub(super) async fn dispatch(c: &Client, call: &Call<'_>) -> Option<CallResult> {
    match call.method {
        "GetKillMail" => {
            let id = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
            Some(killmails(c, None, id, 1, None).await.map(|v| {
                v.items().and_then(|k| k.first().cloned()).unwrap_or(Value::None)
            }))
        }
        _ => None,
    }
}
