//! Binds the client skillMgr2 handler; no specific client module path is established.
//! Uses the client session character ID, not database tables directly.
//! Other skillMgr2 calls are not modelled here.
use super::*;
use marshal::Value;
pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match (call.service, call.method) {
        ("skillMgr2", "GetMySkillHandler") => {
            let me = c.session.int("charid").unwrap_or(0);
            Some(Ok(c.bind(SKILL_HANDLER, Value::tuple([Value::Int(me)]))))
        }
        _ => None,
    }
}
