//! Jump timer service; timers are generated at the current server time.
//! No jump-fatigue state is modelled.
use super::*;
pub(super) async fn dispatch(_c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.method {
        "GetTimers" => Some(Ok(Value::tuple([Value::Int(0), Value::Int(0), Value::Int(crate::client::filetime_now())]))),
        _ => None,
    }
}
