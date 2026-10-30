//! Exploration client-service dispatch.
//! Shared exploration state and data sources are documented in the parent module.

use super::*;

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match (call.service, call.method) {
        ("wormholeMgr", "GetWormholePolarization") => {
            let (char_id, id) = (c.session.int("charid")?, call.args.first().and_then(|v| v.as_int()).unwrap_or(0));
            Some(Ok(polarization(char_id, id).map(|(end, duration)| Value::tuple([Value::Int(end), Value::Int(duration)])).unwrap_or(Value::None)))
        }
        ("wormholeMgr", "WormholeJump") => {
            let id = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
            Some(wormhole_jump(c, id).await)
        }
        _ => None,
    }
}
