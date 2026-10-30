//! Cash balance for the client's `account` service.
//! Reads character wallets through the shared wallet service and DAO.
//! Corporation cash balance requests return zero.
use super::*;

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    Some(match (call.service, call.method) {
        ("account", "GetCashBalance") => {
            let is_corp = call.args.first().and_then(|v| v.as_int()).unwrap_or(0) != 0;
            if is_corp {
                Ok(Value::Float(0.0))
            } else {
                super::super::wallets::balance(&c.db, char_id(c)).await.map(Value::Float)
            }
        }
        _ => return None,
    })
}
