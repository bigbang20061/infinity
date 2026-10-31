//! Effect notifications consumed by client dogma and fxSequencer.
//! Payloads use runtime effect and item data; effect scheduling is handled elsewhere.

use crate::client::filetime_now;
use crate::services::combat::Ctx;
use crate::space::event;
use marshal::Value;

use super::environment;

/// OnGodmaShipEffect(itemID, effectID, time, start, active, environment, startTime,
/// duration, repeat, error, actualStopTime).
#[allow(clippy::too_many_arguments)]
pub(in crate::services) fn ship_effect(cx: &Ctx, module: i64, effect_id: i64, target: Option<i64>, other: Option<Value>, start: bool, started: i64, duration_ms: f64, repeat: i64) {
    let now = filetime_now();
    cx.notify("OnGodmaShipEffect", ship_effect_args(module, effect_id, environment::build(module, cx.char_id, cx.ship_id, target, other, effect_id), start, started, duration_ms, repeat, now));
}

#[allow(clippy::too_many_arguments)]
pub(in crate::services) fn ship_effect_args(module: i64, effect_id: i64, environment: Value, start: bool, started: i64, duration_ms: f64, repeat: i64, now: i64) -> Vec<Value> {
    vec![
        Value::Int(module),
        Value::Int(effect_id),
        Value::Int(now),
        Value::Int(start as i64),
        Value::Int(start as i64),
        environment,
        Value::Int(started),
        Value::Float(duration_ms),
        Value::Int(repeat),
        Value::None,
        if start { Value::None } else { Value::Int(now) },
    ]
}

/// A weapon firing animation (fxSequencer.OnSpecialFX) from `shooter`.
/// `is_offensive` precedes the start flag and comes from the source effect.
#[allow(clippy::too_many_arguments)]
pub(crate) fn fire_fx(stamp: i64, shooter: i64, module: Option<(i64, i32)>, guid: &str, target: Option<i64>, charge: Option<i32>, is_offensive: bool, start: bool, duration_ms: f64, repeat: i64) -> Value {
    event(
        stamp,
        "OnSpecialFX",
        vec![
            Value::Int(shooter),
            module.map(|m| Value::Int(m.0)).unwrap_or(Value::None),
            module.map(|m| Value::Int(m.1 as i64)).unwrap_or(Value::None),
            target.map(Value::Int).unwrap_or(Value::None),
            charge.map(|t| Value::Int(t as i64)).unwrap_or(Value::None),
            Value::str(guid),
            Value::Bool(is_offensive),
            Value::Int(start as i64),
            Value::Int(start as i64),
            Value::Float(duration_ms),
            Value::Int(repeat),
            Value::None,
            Value::Int(0),
            Value::None,
        ],
    )
}

/// One `OnModuleAttributeChanges` entry.
pub(crate) fn attribute_change(owner: i64, item: Value, attribute: i32, new: Value, old: Value) -> Value {
    Value::tuple([
        Value::str("OnModuleAttributeChange"),
        Value::Int(owner),
        item,
        Value::Int(attribute as i64),
        Value::Int(filetime_now()),
        new,
        old,
        Value::None,
    ])
}

#[cfg(test)] mod tests;
