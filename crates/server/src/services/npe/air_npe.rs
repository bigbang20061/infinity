//! AIR NPE state for storylines.client.airnpe, stored in game.air_npe_state.
//! The tutorial itself is unavailable; absent state defaults to skipped.

use super::super::{db_err, Call, CallError, CallResult};
use crate::client::{filetime_now, Client};
use marshal::Value;
use tracing::info;

/// storylines.common.airnpeconstants.AirNpeState.SKIPPED (ACTIVE = 1, COMPLETED = 2).
const STATE_SKIPPED: i16 = 3;

/// A character's tutorial state: stored, else SKIPPED (the tutorial is never offered).
async fn state_of(c: &Client, char_id: i64) -> Result<i16, CallError> {
    Ok(crate::dao::npe::state_of(&c.db, char_id)
        .await
        .map_err(db_err)?
        .map(|r| r.state)
        .unwrap_or(STATE_SKIPPED))
}

async fn set_state(c: &mut Client, char_id: i64, state: i16) -> Result<(), CallError> {
    crate::dao::npe::set_state(&c.db, char_id, state, filetime_now())
        .await
        .map_err(db_err)?;
    // AirNpeController.OnAirNpeStateChanged(character_id, new_state_id)
    c.notify("OnAirNpeStateChanged", vec![Value::Int(char_id), Value::Int(state as i64)]);
    info!(user = c.login.user_id, "character {char_id}: AIR NPE state {state}");
    Ok(())
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    Some(match (call.service, call.method) {
        ("air_npe", "get_air_npe_state") => {
            let char_id = c.session.int("charid")?;
            state_of(c, char_id).await.map(|s| Value::Int(s as i64))
        }
        ("air_npe", "skip_air_npe") => {
            let char_id = c.session.int("charid")?;
            set_state(c, char_id, STATE_SKIPPED).await.map(|_| Value::None)
        }
        _ => return None,
    })
}
