//! Employment history for `corporationSvc`, used by character information.
//! Reads corporation membership history via the corporation service helper.
//! Only the current employment record is represented; transfers are empty.
use super::*;

/// Employment history: the current corporation since the character was created.
async fn employment(c: &Client, id: i64) -> CallResult {
    let history = super::super::corps::employment_rows(&c.db, id).await?;
    Ok(Value::tuple([Value::List(history), Value::list([])]))
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let me = char_id(c);
    Some(match (call.service, call.method) {
        ("corporationSvc", "GetEmployementRecordAndCharacterTransfers") => {
            let id = call.args.first().and_then(|v| v.as_int()).unwrap_or(me);
            employment(c, id).await
        }
        // Just the history, newest first (facWarSvc.IsCharacterInFwCooldown).
        ("corporationSvc", "GetEmploymentRecord") => {
            let id = call.args.first().and_then(|v| v.as_int()).unwrap_or(me);
            super::super::corps::employment_rows(&c.db, id).await.map(Value::List)
        }

        _ => return None,
    })
}
