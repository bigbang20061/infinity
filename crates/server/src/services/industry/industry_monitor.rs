//! `industryMonitor` dispatch for the client industry package, using hangar materials.
//! Corporation industry is not supported.

use super::*;

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let me = c.session.int("charid").unwrap_or(0);
    Some(match (call.service, call.method) {
        // ConnectJob(job.dump()) -> (monitorID, {typeID: quantity available})
        ("industryMonitor", "ConnectJob") => {
            let request = call.args.first().cloned().unwrap_or(Value::None);
            let station = request.get("inputLocation").and_then(|l| field(l, "itemID")).and_then(|v| v.as_int()).or_else(|| request.get("facilityID").and_then(|v| v.as_int()));
            let have = match station {
                Some(s) => match available(&c.db, me, s).await {
                    Ok(h) => h,
                    Err(e) => return Some(Err(e)),
                },
                None => HashMap::new(),
            };
            Ok(Value::tuple([Value::Int(1), Value::Dict(have.into_iter().map(|(t, q)| (Value::Int(t as i64), Value::Int(q))).collect())]))
        }
        ("industryMonitor", "DisconnectJob") => Ok(Value::None),

        _ => return None,
    })
}
