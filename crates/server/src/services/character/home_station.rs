//! Home station lookup for the client's `home_station` service.
//! Reads character home IDs from game data and resolves stations from the startup SDE.
//! Returns None when no matching station exists; fallback stations are unsupported.
use super::*;

pub(in crate::services) struct Station {
    pub station_id: i64,
    pub solar_system_id: i64,
    pub constellation_id: i64,
    pub region_id: i64,
}

pub(in crate::services) async fn home(ex: &(impl crate::db::executor::Executor + ?Sized), sde: &crate::sde::Sde, me: i64) -> Result<Station, CallError> {
    let id = crate::dao::characters::ship::home_station(ex, me).await.map_err(db_err)?;
    let station = id.and_then(|id| sde.map.station(id))
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("home station required row not found")))?;
    let required = |value: Option<i64>| value.ok_or_else(|| CallError::Internal(anyhow::anyhow!("home station required field is null")));
    Ok(Station {
        station_id: station.station_id,
        solar_system_id: required(station.solar_system_id)?,
        constellation_id: required(station.constellation_id)?,
        region_id: required(station.region_id)?,
    })
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let me = char_id(c);
    Some(match (call.service, call.method) {
        ("home_station", "get_home_station") => {
            async {
                let id = crate::dao::characters::social::home_station_id(&c.db, me).await.map_err(db_err)?;
                let Some(r) = id.and_then(|id| c.sde.map.station(id)) else { return Ok(Value::None) };
                let type_id = r.type_id.ok_or_else(|| CallError::Internal(anyhow::anyhow!("home station type_id is null")))?;
                let solar_system_id = r.solar_system_id.ok_or_else(|| CallError::Internal(anyhow::anyhow!("home station solar_system_id is null")))?;
                Ok(keyval([
                    ("id", Value::Int(r.station_id)),
                    ("type_id", Value::Int(type_id)),
                    ("solar_system_id", Value::Int(solar_system_id)),
                    ("is_fallback", Value::Bool(false)),
                ]))
            }.await
        }
        _ => return None,
    })
}
