//! Mining scanner service over the in-memory asteroid state.
//! Scanner range uses ship and rock positions sampled at the same system-actor time.
use super::*;
/// miningScanMgr.perform_scan() -> [(asteroidID, typeID, quantity)] within reach.
pub(super) async fn perform_scan(c: &Client) -> Value {
    let (Some(system), Some(ship)) = (c.session.int("solarsystemid"), c.session.int("shipid")) else { return Value::None };
    let rocks: Vec<(i64, i32, i64)> = state().rocks.iter().filter(|(_, r)| r.system == system).map(|(id, r)| (*id, r.type_id, r.quantity)).collect();
    c.world.with(system, move |s| {
        let me = s.sample_position(ship)?;
        Some(Value::List(
            rocks
                .into_iter()
                .filter(|(id, _, _)| s.sample_position(*id).is_some_and(|pos| dist(pos, me) <= crate::config::game().mining.scanner_range_m))
                .map(|(id, type_id, quantity)| Value::tuple([Value::Int(id), Value::Int(type_id as i64), Value::Int(quantity)]))
                .collect(),
        ))
    }).await.flatten().unwrap_or(Value::None)
}
