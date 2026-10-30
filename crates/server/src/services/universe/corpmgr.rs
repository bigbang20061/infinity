//! corpmgr service; see the parent feature for data sources and limitations.

use super::*;

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let my_corp = c.session.int("corpid").unwrap_or(0);
    let arg_int = |i: usize| call.args.get(i).and_then(|v| v.as_int());
    Some(Ok(match (call.service, call.method) {
        ("corpmgr", "GetCorporations") => return Some(corporation(c, arg_int(0).unwrap_or(my_corp)).await),
        // Show info on a corporation (corporationInfoWindow reads ceoID, stationID, shares...).
        ("corpmgr", "GetPublicInfo") => return Some(corporation(c, arg_int(0).unwrap_or(my_corp)).await),
        ("corpmgr", "GetCorporationIDForCharacter") => {
            let id = arg_int(0).unwrap_or(0);
            let r = crate::dao::characters::crimewatch::corporation(&c.db, id).await;
            match r {
                Ok(r) => r.or_else(|| c.sde.characters.npc_character(id).and_then(|npc| npc.corporation_id)).map(Value::Int).unwrap_or(Value::None),
                Err(e) => return Some(Err(db_err(e))),
            }
        }
        _ => return None,
    }))
}
