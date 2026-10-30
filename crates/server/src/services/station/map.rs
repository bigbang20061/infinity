//! map service; see the parent feature for data sources and limitations.

use super::*;
struct StationInfo {
    station_id: Option<i64>, type_id: Option<i64>, owner_id: Option<i64>, solar_system_id: Option<i64>,
    constellation_id: Option<i64>, region_id: Option<i64>, operation_id: Option<i64>,
    x: Option<f64>, y: Option<f64>, z: Option<f64>, security: Option<f64>,
}
impl StationInfo {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self { station_id: row.get("station_id"), type_id: row.get("type_id"), owner_id: row.get("owner_id"),
            solar_system_id: row.get("solar_system_id"), constellation_id: row.get("constellation_id"),
            region_id: row.get("region_id"), operation_id: row.get("operation_id"),
            x: row.get("x"), y: row.get("y"), z: row.get("z"), security: row.get("security") }
    }
}
/// Every NPC station as a rowset; the client indexes it by stationID (uisvc.GetStation).
async fn station_info(c: &Client) -> Result<Value, CallError> {
    let rows = c
        .db
        .query(
            "SELECT station_id::bigint, type_id::bigint, owner_id::bigint, solar_system_id::bigint, constellation_id::bigint,
                    region_id::bigint, operation_id::bigint, x, y, z, security
             FROM sde.sta_stations ORDER BY station_id",
            &[],
        )
        .await
        .map_err(db_err)?;
    let lines = rows
        .iter()
        .map(|r| {
            let r = StationInfo::from_row(r);
            let mut l: Vec<Value> = [r.station_id, r.type_id, r.owner_id, r.solar_system_id, r.constellation_id, r.region_id, r.operation_id]
                .into_iter().map(|v| v.map(Value::Int).unwrap_or(Value::None)).collect();
            l.extend([r.x, r.y, r.z, r.security].into_iter().map(|v| Value::Float(v.unwrap_or(0.0))));
            l.insert(1, Value::unicode(""));
            l
        })
        .collect();
    Ok(rowset(
        &["stationID", "stationName", "stationTypeID", "ownerID", "solarSystemID", "constellationID", "regionID", "operationID", "x", "y", "z", "security"],
        lines,
    ))
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    Some(Ok(match (call.service, call.method) {
        ("map", "GetStationInfo") => match station_info(c).await {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        },
        _ => return None,
    }))
}
