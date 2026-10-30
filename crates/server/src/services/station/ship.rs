//! Ship dirt timestamp service; no persistent dirt state is modelled.
use super::*;
pub(super) async fn dispatch(_c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.method {
        "GetDirtTimestamp" => Some(Ok(Value::Int(crate::client::filetime_now()))),
        _ => None,
    }
}
