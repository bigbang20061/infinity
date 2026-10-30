//! standingMgr answers standingsvc using game.standings; corporation standings are not modelled.
use crate::services::{db_err, rowset, Call, CallResult};
use crate::client::Client;
use marshal::Value;

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let me = c.session.int("charid").unwrap_or(0);
    Some(match call.method {
        "GetCharStandings" => match crate::dao::characters::standings::list(&c.db, me).await {
            Ok(rows) => Ok(rowset(&["fromID", "standing"], rows.iter().map(|r| vec![Value::Int(r.from_id), Value::Float(r.standing)]).collect())),
            Err(e) => Err(db_err(e)),
        },
        "GetCorpStandings" => Ok(rowset(&["fromID", "standing"], vec![])),
        "GetStandingTransactions" | "GetStandingCompositions" => Ok(Value::list([])),
        _ => return None,
    })
}
