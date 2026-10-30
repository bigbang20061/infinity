//! Macho network time and configuration service; values come from server configuration.
use super::*;
pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.method {
        "GetTime" => Some(Ok(Value::Int(crate::client::filetime_now()))),
        "GetGlobalConfig" => Some(Ok(c.cfg.global_config())),
        _ => None,
    }
}
