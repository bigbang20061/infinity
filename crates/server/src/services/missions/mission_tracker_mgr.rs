//! Mission tracker service.
use super::*;
// ---------------------------------------------------------------- mission tracker

/// Is the pilot's ship at the mission site?
async fn at_site(c: &Client, m: &Mission) -> bool {
    let (Some(system), Some(ship)) = (c.session.int("solarsystemid"), c.session.int("shipid")) else { return false };
    if system != m.system_id {
        return false;
    }
    let pos = m.pos;
    c.world.with(system, move |s| s.ball(ship).is_some_and(|b| dist(b.pos, pos) < SITE_RADIUS)).await.unwrap_or(false)
}

/// The steps of a kill mission, as missionObjectiveData objective items (strings).
fn tracker_objectives(m: &Mission) -> [Vec<String>; 3] {
    [vec!["TravelTo".into(), m.system_id.to_string()], vec!["DestroyAll".into()], vec!["AllObjectivesComplete".into(), m.agent_id.to_string()]]
}

/// The step the pilot is on, or None before the mission is accepted.
async fn tracker_current(c: &Client, m: &Mission) -> Option<Vec<String>> {
    // Only kill missions have a site to steer to; the rest show "read the journal".
    if m.state != STATE_ACCEPTED || m.kind != Kind::Kill {
        return None;
    }
    let [travel, destroy, complete] = tracker_objectives(m);
    Some(if objective_done(c, m).await {
        complete
    } else if at_site(c, m).await {
        destroy
    } else {
        travel
    })
}

fn strings(items: Vec<String>) -> Value {
    Value::list(items.into_iter().map(Value::str))
}

/// missionTrackerMgr: the info panel's mission objectives (missionObjectivesTracker).
pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let agent_id = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
    let current = match mission_with(c, agent_id).await {
        Ok(m) => m,
        Err(e) => return Some(Err(e)),
    };
    Some(Ok(match call.method {
        "GetMissionInfoItems" => match current {
            Some(m) => tracker_current(c, &m).await.map(strings).unwrap_or(Value::None),
            None => Value::None,
        },
        "GetAllMissionObjectives" => match current {
            Some(m) if m.state == STATE_ACCEPTED && m.kind == Kind::Kill => Value::list(tracker_objectives(&m).into_iter().map(strings)),
            _ => Value::list([]),
        },
        "IsInActiveDungeonID" => Value::Bool(match current {
            Some(m) => at_site(c, &m).await,
            None => false,
        }),
        // UpdateAllMissions(agentIDs): answered with OnMissionsUpdated.
        "UpdateAllMissions" => {
            let agents: Vec<i64> = match call.args.first() {
                Some(Value::List(l) | Value::Tuple(l)) => l.iter().filter_map(|v| v.as_int()).collect(),
                _ => vec![],
            };
            let mut updates = vec![];
            for agent_id in agents {
                let info = match mission_with(c, agent_id).await {
                    Ok(Some(m)) => tracker_current(c, &m).await.map(strings).unwrap_or(Value::None),
                    Ok(None) => Value::None,
                    Err(e) => return Some(Err(e)),
                };
                updates.push(Value::dict([("agentID", Value::Int(agent_id)), ("info", info)]));
            }
            c.notify("OnMissionsUpdated", vec![Value::list(updates)]);
            Value::None
        }
        _ => return None,
    }))
}

