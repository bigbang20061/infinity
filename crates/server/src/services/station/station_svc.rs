//! stationSvc service; see the parent feature for data sources and limitations.

use super::*;

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let int = Value::Int;
    Some(Ok(match (call.service, call.method) {
        ("stationSvc", "GetStationItemBits") => {
            let station = c.session.int("stationid").unwrap_or(0);
            let Some(r) = c.sde.map.station(station as i32 as i64) else {
                return Some(Err(CallError::Internal(anyhow::anyhow!("station {station} not found"))));
            };
            Value::list([r.owner_id, Some(r.station_id), r.operation_id, r.type_id].into_iter().map(|v| v.map(int).unwrap_or(Value::None)))
        }
        _ => return None,
    }))
}
