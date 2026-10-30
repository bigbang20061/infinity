//! map service; see the parent feature for data sources and limitations.

use super::*;
/// Server default industry cost modifier for each solar system.
const INDUSTRY_COST_MODIFIER: f64 = 0.001;
struct StationCount { solar_system_id: i64, count: i64 }
impl StationCount {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self { solar_system_id: row.get("solar_system_id"), count: row.get("count") }
    }
}
struct SolarSystem { solar_system_id: i64 }
impl SolarSystem {
    fn from_row(row: &tokio_postgres::Row) -> Self { Self { solar_system_id: row.get("solar_system_id") } }
}

async fn station_counts(c: &Client) -> Result<Value, CallError> {
    let rows = c
        .db
        .query("SELECT solar_system_id::bigint, count(*) FROM sde.sta_stations GROUP BY solar_system_id", &[])
        .await
        .map_err(db_err)?;
    Ok(Value::List(rows.iter().map(|r| { let r = StationCount::from_row(r); Value::tuple([Value::Int(r.solar_system_id), Value::Int(r.count)]) }).collect()))
}

async fn industry_cost_modifiers(c: &Client) -> Result<Value, CallError> {
    let rows = c.db.query("SELECT solar_system_id::bigint FROM sde.map_solar_systems", &[]).await.map_err(db_err)?;
    Ok(Value::Dict(rows.iter().map(|r| (Value::Int(SolarSystem::from_row(r).solar_system_id), Value::Float(INDUSTRY_COST_MODIFIER))).collect()))
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match (call.service, call.method) {
        ("map", "GetStationCount") => Some(station_counts(c).await),
        ("map", "GetIndustryCostModifier") => Some(industry_cost_modifiers(c).await),
        _ => None,
    }
}
