//! Home station lookup for the client's `home_station` service.
//! Joins `game.characters` and `sde.sta_stations` through the character DAO.
//! Returns None when no matching station exists; fallback stations are unsupported.
use super::*;

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let me = char_id(c);
    Some(match (call.service, call.method) {
        ("home_station", "get_home_station") => {
            let r = crate::dao::characters::social::home_station_for_service(&c.db, me).await.map_err(db_err);
            r.map(|r| match r {
                Some(r) => keyval([
                    ("id", Value::Int(r.station_id)),
                    ("type_id", Value::Int(r.type_id)),
                    ("solar_system_id", Value::Int(r.solar_system_id)),
                    ("is_fallback", Value::Bool(false)),
                ]),
                None => Value::None,
            })
        }
        _ => return None,
    })
}
