//! Client timer and security state methods for `crimewatch`.
//! Mirrors `eve/client/script/ui/services/crimewatchSvc.py`.
//! Security uses `game.characters`; timers and engagements are in memory.
//! Security transaction history is not modelled; flags are memory-only.

use super::*;

/// `GetClientStates` -> (combat timers, engagements, (criminals, suspects) in the system, safety level).
async fn client_states(c: &Client) -> Value {
    let me = c.session.int("charid").unwrap_or(0);
    let here: HashSet<i64> = match c.session.int("solarsystemid") {
        Some(system) => pilots_in(&c.world, system).await.into_iter().collect(),
        None => HashSet::new(),
    };
    let now = filetime_now();
    let st = state();
    let pilot = st.pilots.get(&me);
    let running = |until: Option<i64>, on: i64, off: i64| match until.filter(|u| *u > now) {
        Some(u) => timer(on, Some(u)),
        None => timer(off, None),
    };
    let timers = Value::tuple([
        running(pilot.map(|p| p.weapons_until), WEAPONS_TIMER, WEAPONS_IDLE),
        running(pilot.map(|p| p.pvp_until), PVP_TIMER, PVP_IDLE),
        timer(NPC_IDLE, None),
        match pilot {
            Some(p) => { let (_, state, until) = effective_flag(p, now); timer(state, until) },
            None => timer(CRIMINAL_IDLE, None),
        },
        timer(DISAPPROVAL_IDLE, None),
    ]);
    let engagements = Value::Dict(st.engagements.iter().filter(|((a, _), until)| *a == me && **until > now).map(|((_, b), until)| (Value::Int(*b), Value::Int(*until))).collect());
    let criminals: Vec<Value> = st.pilots.iter().filter(|(ch, p)| p.criminal_until > now && here.contains(ch)).map(|(ch, _)| Value::Int(*ch)).collect();
    let suspects: Vec<Value> = st.pilots.iter().filter(|(ch, p)| p.suspect_until > now && p.criminal_until <= now && here.contains(ch)).map(|(ch, _)| Value::Int(*ch)).collect();
    Value::tuple([timers, engagements, Value::tuple([Value::List(criminals), Value::List(suspects)]), Value::Int(SAFETY_LEVEL_FULL)])
}

pub(super) async fn dispatch(c: &Client, call: &Call<'_>) -> Option<CallResult> {
    let int = |i: usize| call.args.get(i).and_then(|v| v.as_int());
    Some(match call.method {
        "GetClientStates" => Ok(client_states(c).await),
        "GetMySecurityStatus" => security_status(&c.db, c.session.int("charid").unwrap_or(0)).await.map(Value::Float),
        "GetCharacterSecurityStatus" => security_status(&c.db, int(0).unwrap_or(0)).await.map(Value::Float),
        "GetSecurityStatusTransactions" => Ok(Value::list([])),
        _ => return None,
    })
}
