//! stationSvc service; see the parent feature for data sources and limitations.

use super::*;
struct StationItemBits { owner_id: Option<i64>, station_id: Option<i64>, operation_id: Option<i64>, type_id: Option<i64> }
impl StationItemBits {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self { owner_id: row.get("owner_id"), station_id: row.get("station_id"), operation_id: row.get("operation_id"), type_id: row.get("type_id") }
    }
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let int = Value::Int;
    Some(Ok(match (call.service, call.method) {
        ("stationSvc", "GetStationItemBits") => {
            let station = c.session.int("stationid").unwrap_or(0);
            let r = match c
                .db
                .query_one(
                    "SELECT owner_id::bigint, station_id::bigint, operation_id::bigint, type_id::bigint FROM sde.sta_stations WHERE station_id = $1",
                    &[&(station as i32)],
                )
                .await
            {
                Ok(r) => r,
                Err(e) => return Some(Err(db_err(e))),
            };
            let r = StationItemBits::from_row(&r);
            Value::list([r.owner_id, r.station_id, r.operation_id, r.type_id].into_iter().map(|v| v.map(int).unwrap_or(Value::None)))
        }
        _ => return None,
    }))
}
