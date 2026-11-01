//! `facilityManager` dispatch for the client industry package, using SDE station facilities.
//! NPC facility tax settings are read-only.

use super::*;

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let me = c.session.int("charid").unwrap_or(0);
    let int = |i: usize| call.args.get(i).and_then(|v| v.as_int()).unwrap_or(0);
    Some(match (call.service, call.method) {
        ("facilityManager", "GetFacilities") => {
            let region = c.session.int("regionid").unwrap_or(0);
            Ok(Value::list(c.sde.map.stations().filter(|s| s.region_id == Some(region)).filter_map(|s| Facility::from_station(&c.sde, s)).map(|f| f.data())))
        }
        ("facilityManager", "GetFacility") => facility(&c.sde, int(0)).map(|f| f.data()),
        ("facilityManager", "GetFacilitiesByID") => {
            let ids = value_id_list(call.args.first());
            let mut out = vec![];
            for id in ids {
                match facility(&c.sde, id) {
                    Ok(f) => out.push(f.data()),
                    Err(CallError::User { .. }) => {}
                    Err(e) => return Some(Err(e)),
                }
            }
            Ok(Value::List(out))
        }
        // GetFacilityLocations(facilityID, ownerID): the owner's item hangar there.
        ("facilityManager", "GetFacilityLocations") => {
            let owner = call.args.get(1).and_then(|v| v.as_int()).unwrap_or(me);
            match facility(&c.sde, int(0)) {
                Ok(f) => authorization::locations(&c.db, &c.sde, me, owner, &f).await.map(|locations| Value::list(locations.iter().map(authorization::Location::value))),
                Err(CallError::User { .. }) => Ok(Value::list([])),
                Err(e) => Err(e),
            }
        }
        ("facilityManager", "GetMaxActivityModifiers") => {
            Ok(Value::Dict([MANUFACTURING, RESEARCH_TIME, RESEARCH_MATERIAL, COPYING, INVENTION].iter().map(|a| (Value::Int(*a as i64), Value::Float(1.0))).collect()))
        }
        ("facilityManager", "GetFacilityTaxes") => facility(&c.sde, int(0)).map(|f| f.tax_settings()),
        ("facilityManager", "SetFacilityTaxes") => facility(&c.sde, int(0))
            .and_then(|_| Err(notify_error(messages::industry::NPC_TAX_IMMUTABLE))),

        _ => return None,
    })
}
