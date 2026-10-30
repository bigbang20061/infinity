//! Epic arc journal messages for `agentMgr`, as used by the epic arc controller.
//! Mirrors `evemissions.common.epic_arcs_data` message types and reads `sde_native.missions`.
//! Other agent and mission operations are handled outside this service leaf.

use super::super::{db_err, Call, CallResult};
use crate::client::Client;
use marshal::Value;
use std::sync::Mutex;

/// {messageType: {missionID: messageID}} for every epic arc journal message
/// (evemissions.common.epic_arcs_data MESSAGE_*). The epic arc controller asks once per
/// mission node, so the answer is built once.
async fn epic_arc_messages(c: &Client) -> CallResult {
    static CACHE: Mutex<Option<Value>> = Mutex::new(None);
    if let Some(v) = CACHE.lock().unwrap().clone() {
        return Ok(v);
    }
    let rows = c.db.query(
        "SELECT k, m.key::int AS mission_id, (m.data->'messages'->>k)::bigint AS message_id
             FROM sde_native.missions m, jsonb_object_keys(m.data->'messages') k
             WHERE k LIKE 'messages.epicMission.journalText.%'
             ORDER BY k", &[]).await.map_err(db_err)?;
    let mut by_type: Vec<(String, Vec<(Value, Value)>)> = vec![];
    for r in rows {
        let kind: String = r.get("k");
        let entry = (Value::Int(r.get::<_, i32>("mission_id") as i64), Value::Int(r.get("message_id")));
        match by_type.last_mut() {
            Some((k, list)) if *k == kind => list.push(entry),
            _ => by_type.push((kind, vec![entry])),
        }
    }
    let v = Value::Dict(by_type.into_iter().map(|(k, list)| (Value::str(k), Value::Dict(list))).collect());
    *CACHE.lock().unwrap() = Some(v.clone());
    Ok(v)
}

pub(super) async fn dispatch(c: &Client, call: &Call<'_>) -> Option<CallResult> {
    match call.method {
        "GetMessagesForEpicArcMissions" => Some(epic_arc_messages(c).await),
        _ => None,
    }
}
