//! loginRewardFacilities clone grade for the login rewards client.
//! This fixed answer uses no data source and does not model alpha clones.
use super::*;

pub(super) async fn dispatch(_c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    Some(match (call.service, call.method) {
        ("loginRewardFacilities", "get_my_clone_grade") => Ok(Value::Int(1)), // CLONE_STATE_OMEGA

        _ => return None,
    })
}
