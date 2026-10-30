//! Fixed corporation fitting responses, mirroring corpFittingMgr.
//! Corporation fittings are not modeled and return empty dictionaries.
use crate::services::{Call, CallResult};
use marshal::Value;
pub(super) async fn dispatch(call: &Call<'_>) -> Option<CallResult> {
    match call.method { "GetFittings" | "GetCommunityFittings" => Some(Ok(Value::Dict(vec![]))), _ => None }
}
