//! map service; see the parent feature for data sources and limitations.

use super::*;
/// Server default industry cost modifier for each solar system.
const INDUSTRY_COST_MODIFIER: f64 = 0.001;
async fn station_counts(c: &Client) -> Result<Value, CallError> {
    let mut counts = std::collections::BTreeMap::new();
    for station in c.sde.map.stations() {
        let system = station.solar_system_id.ok_or_else(|| CallError::Internal(anyhow::anyhow!("station has no solar system")))?;
        *counts.entry(system).or_insert(0_i64) += 1;
    }
    Ok(Value::List(counts.into_iter().map(|(system, count)| Value::tuple([Value::Int(system), Value::Int(count)])).collect()))
}

async fn industry_cost_modifiers(c: &Client) -> Result<Value, CallError> {
    Ok(Value::Dict(c.sde.map.systems().map(|r| (Value::Int(r.solar_system_id), Value::Float(INDUSTRY_COST_MODIFIER))).collect()))
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match (call.service, call.method) {
        ("map", "GetStationCount") => Some(station_counts(c).await),
        ("map", "GetIndustryCostModifier") => Some(industry_cost_modifiers(c).await),
        _ => None,
    }
}
