//! Recent character kill and loss reports for `corpRegistry`.
//! Answers the client corporation registry's recent kill and loss calls.
//! Reads `game.killmails` through the shared crimewatch query.
//! Alliance, faction and war fields not stored by the server are returned empty.

use super::*;

pub(super) async fn dispatch(c: &Client, call: &Call<'_>) -> Option<CallResult> {
    let int = |i: usize| call.args.get(i).and_then(|v| v.as_int());
    let me = c.session.int("charid").unwrap_or(0);
    Some(match call.method {
        "GetRecentKills" => killmails(c, Some(true), me, int(0).unwrap_or(10), int(1)).await,
        "GetRecentLosses" => killmails(c, Some(false), me, int(0).unwrap_or(10), int(1)).await,
        _ => return None,
    })
}
