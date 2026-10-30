//! activityMgr service; see the parent feature for data sources and limitations.

use super::*;

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let int = Value::Int;
    Some(Ok(match (call.service, call.method) {
        ("activityMgr", "GetActivitiesPopState") => {
            let me = c.session.int("charid").unwrap_or(0);
            match crate::dao::characters::routing::activities_pop_state(&c.db, me).await {
                Ok(r) => int(r.map(i64::from).unwrap_or(1)),
                Err(e) => return Some(Err(db_err(e))),
            }
        }
        ("activityMgr", "SetActivitiesPopState") => {
            let me = c.session.int("charid").unwrap_or(0);
            let state = call.args.first().and_then(|v| v.as_int()).unwrap_or(1) as i32;
            match crate::dao::characters::routing::set_activities_pop_state(&c.db, me, state).await {
                Ok(_) => Value::Bool(true),
                Err(e) => return Some(Err(db_err(e))),
            }
        }
        // No activity badges are shown, so there's nothing to clear.
        _ => return None,
    }))
}
