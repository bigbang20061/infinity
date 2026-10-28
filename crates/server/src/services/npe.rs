//! The new player experience (storylines.client.airnpe, nodegraph.client).
//!
//! This build's opening tutorial ("AIR NPE") is a set of node graphs: server graph 49
//! ([AIR] 0.0 - Server Master) with ~40 server subgraphs, and client graph 716 with ~88
//! client subgraphs (both in sde_native.nodegraphs). The server graphs spawn the
//! tutorial's dungeons (9103 station wreckage, 9105, the mining dungeon), allocate a
//! void-space pocket, and script the guide NPC and enemy waves through dungeon behavior
//! blackboards. Those dungeon layouts and behavior trees never shipped with the client
//! or the SDE, so the tutorial can't be run: `is_air_npe_enabled` stays off and new
//! characters start in their school station as if they had skipped it.
//!
//! What the client still needs is each character's tutorial state: anything but
//! COMPLETED or SKIPPED reads as "tutorial unfinished", which hides the HUD's lock-and-fire
//! buttons and the amplification matrix slot (shipUI, hudButtonsCont).

use super::{Call, CallError, CallResult};
use crate::client::{filetime_now, Client};
use marshal::Value;
use tracing::{debug, info};

fn db_err(e: tokio_postgres::Error) -> CallError {
    CallError::Internal(e.into())
}

/// storylines.common.airnpeconstants.AirNpeState.SKIPPED (ACTIVE = 1, COMPLETED = 2).
const STATE_SKIPPED: i16 = 3;

/// A character's tutorial state: stored, else SKIPPED (the tutorial is never offered).
async fn state_of(c: &Client, char_id: i64) -> Result<i16, CallError> {
    Ok(c.db
        .client
        .query_opt("SELECT state FROM game.air_npe_state WHERE character_id = $1", &[&char_id])
        .await
        .map_err(db_err)?
        .map(|r| r.get(0))
        .unwrap_or(STATE_SKIPPED))
}

async fn set_state(c: &mut Client, char_id: i64, state: i16) -> Result<(), CallError> {
    c.db.client
        .execute(
            "INSERT INTO game.air_npe_state (character_id, state, changed_at) VALUES ($1, $2, $3)
             ON CONFLICT (character_id) DO UPDATE SET state = EXCLUDED.state, changed_at = EXCLUDED.changed_at",
            &[&char_id, &state, &filetime_now()],
        )
        .await
        .map_err(db_err)?;
    // AirNpeController.OnAirNpeStateChanged(character_id, new_state_id)
    c.notify("OnAirNpeStateChanged", vec![Value::Int(char_id), Value::Int(state as i64)]);
    info!(user = c.login.user_id, "character {char_id}: AIR NPE state {state}");
    Ok(())
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    Some(match (call.service, call.method) {
        ("air_npe", "get_air_npe_state") => {
            let char_id = c.session.int("charid")?;
            state_of(c, char_id).await.map(|s| Value::Int(s as i64))
        }
        ("air_npe", "skip_air_npe") => {
            let char_id = c.session.int("charid")?;
            set_state(c, char_id, STATE_SKIPPED).await.map(|_| Value::None)
        }
        // Client graphs talk to the server graph that started them; none run here, so
        // there is no server graph instance to deliver to (nodegraph.client.graph).
        ("node_graph", "process_client_graph_message") | ("node_graph", "process_client_blackboard_request") => {
            debug!(user = c.login.user_id, "node_graph::{} with no server graph: {:?}", call.method, call.args);
            Ok(Value::None)
        }
        ("node_graph", "qa_subscribe") => Ok(Value::list([])),
        ("node_graph", m) if m.starts_with("qa_") => Ok(Value::None),
        _ => return None,
    })
}

