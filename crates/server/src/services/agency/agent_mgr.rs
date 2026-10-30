//! Epic arc journal messages for `agentMgr`, as used by the epic arc controller.
//! Mirrors `evemissions.common.epic_arcs_data` message types using the shared SDE.
//! Other agent and mission operations are handled outside this service leaf.

use super::super::{Call, CallResult};
use crate::client::Client;
use marshal::Value;

/// {messageType: {missionID: messageID}} for every epic arc journal message
/// (evemissions.common.epic_arcs_data MESSAGE_*). The epic arc controller asks once per
/// mission node; the projected data is shared and the wire value is built per call.
async fn epic_arc_messages(c: &Client) -> CallResult {
    let mut by_type: Vec<(String, Vec<(Value, Value)>)> = vec![];
    for r in c.sde.missions.journal() {
        let kind = r.key.clone();
        let entry = (Value::Int(i64::from(r.mission_id)), Value::Int(r.message_id.expect("epic arc journal message_id is NULL")));
        match by_type.last_mut() {
            Some((k, list)) if *k == kind => list.push(entry),
            _ => by_type.push((kind, vec![entry])),
        }
    }
    Ok(Value::Dict(by_type.into_iter().map(|(k, list)| (Value::str(k), Value::Dict(list))).collect()))
}

pub(super) async fn dispatch(c: &Client, call: &Call<'_>) -> Option<CallResult> {
    match call.method {
        "GetMessagesForEpicArcMissions" => Some(epic_arc_messages(c).await),
        _ => None,
    }
}
