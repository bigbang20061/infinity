//! Analyzer effect validation and timed client notifications.
//!
//! Uses the item database and SDE dogma data; hacking gameplay remains in the
//! hacking service. Timers run on the solar-system actor.

use crate::client::{filetime_now, Client};
use crate::consts::EFFECT_HACKING;
use crate::messages;
use crate::services::{combat, notify_error, CallResult};
use crate::space::{ms_to_seconds, seconds_to_ms};
use marshal::Value;

use super::{lifecycle, notify, registry, EffectKind};

/// Validate the actual item before emitting an analyzer effect.
pub(crate) async fn start_effect(c: &mut Client, module: i64, target: i64, duration_ms: f64) -> CallResult {
    let Some(cx) = combat::ctx(c) else {
        return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
    };
    lifecycle::subject(c, cx.ship_id, module, EFFECT_HACKING, true).await?;
    if registry::kind(&c.sde, EFFECT_HACKING) != Some(EffectKind::Hacking) {
        return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
    }
    let started = filetime_now();
    notify::ship_effect(&cx, module, EFFECT_HACKING as i64, Some(target), None, true, started, duration_ms, 0);
    let world = cx.world.clone();
    world.schedule(cx.system_id, seconds_to_ms(ms_to_seconds(duration_ms.max(0.0))), move |s| {
        s.defer(async move {
            notify::ship_effect(&cx, module, EFFECT_HACKING as i64, Some(target), None, false, started, duration_ms, 0);
        });
    });
    Ok(Value::None)
}

#[cfg(test)] mod tests;
