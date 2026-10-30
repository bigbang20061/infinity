//! Fixed alliance fitting responses, mirroring allianceFittingMgr.
//! Alliance fittings are not modeled and return an empty dictionary.
use crate::services::{Call, CallResult};
use marshal::Value;
pub(super) async fn dispatch(call: &Call<'_>) -> Option<CallResult> {
    match call.method { "GetFittings" => Some(Ok(Value::Dict(vec![]))), _ => None }
}
