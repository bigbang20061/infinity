//! Fleet manager (eve/client/script/parklife/fleetSvc.py).
//! Operates on process-local fleet state; fleet advertising is not supported.

use super::*;

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.method {
        "ForceLeaveFleet" => {
            logged_off(&c.world, c.session.int("charid").unwrap_or(0));
            Some(Ok(Value::None))
        }
        _ => None,
    }
}
