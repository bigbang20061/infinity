//! map service; see the parent feature for data sources and limitations.

use super::*;
/// Every NPC station as a rowset; the client indexes it by stationID (uisvc.GetStation).
async fn station_info(c: &Client) -> Result<Value, CallError> {
    let lines = c.sde.map.stations()
        .map(|r| {
            let mut l: Vec<Value> = [Some(r.station_id), r.type_id, r.owner_id, r.solar_system_id, r.constellation_id, r.region_id, r.operation_id]
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
