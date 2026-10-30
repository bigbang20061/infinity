//! Client telemetry delivery for `alert`, mirroring `alert.__BeanDeliveryBoy`.
//! Counter payloads come from the client; no database tables are consulted.
//! Counters are logged, not persisted or otherwise processed.

use super::super::{Call, CallResult};
use crate::client::Client;
use marshal::Value;
use tracing::debug;

pub(super) fn dispatch(c: &Client, call: &Call<'_>) -> Option<CallResult> {
    match call.method {
        // alert.__BeanDeliveryBoy: {counterID: (count, time)}, or the zlib-compressed group form.
        "BeanDelivery" | "GroupBeanDelivery" => {
            let entries = match call.args.first() {
                Some(Value::Dict(d)) => d.len(),
                _ => 0,
            };
            debug!(user = c.login.user_id, "client {}: {entries} counters", call.method);
            Some(Ok(Value::None))
        }
        _ => None,
    }
}
