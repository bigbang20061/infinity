//! Fleets (fleetObjectHandler, bound per fleet; eve/client/script/parklife/fleetSvc.py).
//!
//! A fleet lives in memory while it has members: wings of squads, a leader, a MOTD and the
//! invitations out. Creating one (`CreateFleet` then `Init`) makes the creator its leader in
//! its first squad; an invited pilot accepts through their own bound fleet object. Members
//! carry the fleet in their session (fleetid, wingid, squadid, fleetrole), which also puts
//! them in the fleet chat channel; joins, departures and changes are pushed to every member
//! (OnFleetJoin, OnFleetLeave, OnFleetMemberChanged...). A pilot who logs off leaves.
//! The fleet finder returns an empty advert dictionary: fleets are not advertised.
//! Fleet warps, boosts and advertising are not supported.

use super::{keyval, notify_error, value_text, Call, CallError, CallResult};
use crate::client::Client;
use crate::messages;
use crate::space::World;
use marshal::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use tracing::info;

/// Client evefleet.const fleet leader role.
const ROLE_LEADER: i64 = 1;
/// Client evefleet.const fleet member role.
const ROLE_MEMBER: i64 = 4;
/// Client evefleet.const no fleet job.
const JOB_NONE: i64 = 0;
/// Client evefleet.const fleet creator job.
const JOB_CREATOR: i64 = 2;
/// Client evefleet.const absent wing or squad ID.
const NONE_ID: i64 = -1;
/// Server fleet policy: maximum fleet membership.
pub(super) const MAX_MEMBERS: usize = 256;
/// Server fleet policy: maximum wing and squad name length.
const MAX_NAME_LENGTH: usize = 20;
/// Server allocation policy: first process-local fleet, wing or squad ID.
const FIRST_FLEET_ID: i64 = 1;

static NEXT_ID: AtomicI64 = AtomicI64::new(FIRST_FLEET_ID);

#[derive(Clone)]
struct Member {
    wing: i64,
    squad: i64,
    role: i64,
    job: i64,
    ship_type: i64,
}

struct Squad {
    id: i64,
    name: String,
}

struct Wing {
    id: i64,
    name: String,
    squads: Vec<Squad>,
}

struct Fleet {
    members: HashMap<i64, Member>,
    wings: Vec<Wing>,
    motd: String,
    /// Invited pilot -> (wing, squad, role) they'll join as.
    invites: HashMap<i64, (i64, i64, i64)>,
}

#[derive(Default)]
struct State {
    fleets: HashMap<i64, Fleet>,
    /// Pilot -> fleet.
    of: HashMap<i64, i64>,
}

fn state() -> MutexGuard<'static, State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(Default::default).lock().unwrap()
}

pub(crate) fn same_fleet(a: i64, b: i64) -> bool {
    let state = state();
    state.of.get(&a).is_some_and(|fleet| state.of.get(&b) == Some(fleet))
}

fn next_id() -> i64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

fn member_value(char_id: i64, m: &Member) -> Value {
    keyval([
        ("charID", Value::Int(char_id)),
        ("wingID", Value::Int(m.wing)),
        ("squadID", Value::Int(m.squad)),
        ("role", Value::Int(m.role)),
        ("job", Value::Int(m.job)),
        ("shipTypeID", Value::Int(m.ship_type)),
        ("memberOptOuts", Value::None),
    ])
}

fn wings_value(f: &Fleet) -> Value {
    Value::Dict(
        f.wings
            .iter()
            .map(|w| {
                let squads = Value::Dict(w.squads.iter().map(|s| (Value::Int(s.id), keyval([("squadID", Value::Int(s.id)), ("name", Value::unicode(s.name.as_str()))]))).collect());
                (Value::Int(w.id), keyval([("wingID", Value::Int(w.id)), ("name", Value::unicode(w.name.as_str())), ("squads", squads)]))
            })
            .collect(),
    )
}

/// Tell everyone in the fleet (but `except`).
fn broadcast(world: &World, f: &Fleet, except: Option<i64>, event: &str, args: Vec<Value>) {
    for ch in f.members.keys().filter(|ch| Some(**ch) != except) {
        world.notify_pilot(*ch, event, args.clone());
    }
}

fn session_pairs(fleet: Option<i64>, m: Option<&Member>) -> Vec<Value> {
    let pair = |k: &str, v: Value| Value::tuple([Value::str(k), v]);
    let opt = |v: Option<i64>| v.map(Value::Int).unwrap_or(Value::None);
    vec![
        pair("fleetid", opt(fleet)),
        pair("wingid", opt(m.map(|m| m.wing))),
        pair("squadid", opt(m.map(|m| m.squad))),
        pair("fleetrole", opt(m.map(|m| m.role))),
    ]
}

/// The fleet a bound call is for.
fn bound_fleet(call: &Call<'_>) -> i64 {
    call.bind_params.and_then(|p| p.as_int().or_else(|| p.items().and_then(|t| t.first()?.as_int()))).unwrap_or(0)
}

/// Remove a member (leaving, kicked or logged off); an empty fleet goes away and a
/// leaderless one passes the lead on.
fn remove_member(world: &World, fleet_id: i64, char_id: i64) {
    let mut st = state();
    st.of.remove(&char_id);
    let Some(f) = st.fleets.get_mut(&fleet_id) else { return };
    let Some(old) = f.members.remove(&char_id) else { return };
    world.notify_pilot(char_id, "__session", session_pairs(None, None));
    world.notify_pilot(char_id, "OnFleetLeave", vec![Value::Int(char_id)]);
    broadcast(world, f, None, "OnFleetLeave", vec![Value::Int(char_id)]);
    if f.members.is_empty() {
        st.fleets.remove(&fleet_id);
        info!("fleets: {fleet_id} disbanded");
        return;
    }
    if old.role == ROLE_LEADER {
        if let Some((&next, m)) = f.members.iter_mut().next() {
            let before = m.clone();
            m.role = ROLE_LEADER;
            let after = m.clone();
            world.notify_pilot(next, "__session", session_pairs(Some(fleet_id), Some(&after)));
            broadcast(world, f, None, "OnFleetMemberChanged", changed(next, fleet_id, &before, &after, false));
        }
    }
}

fn changed(char_id: i64, fleet_id: i64, before: &Member, after: &Member, only: bool) -> Vec<Value> {
    vec![
        Value::Int(char_id),
        Value::Int(fleet_id),
        Value::Int(before.wing),
        Value::Int(before.squad),
        Value::Int(before.role),
        Value::Int(before.job),
        Value::None,
        Value::Int(after.wing),
        Value::Int(after.squad),
        Value::Int(after.role),
        Value::Int(after.job),
        Value::None,
        Value::Bool(only),
    ]
}

/// A pilot logged off: they leave their fleet.
pub fn logged_off(world: &World, char_id: i64) {
    let fleet = state().of.get(&char_id).copied();
    if let Some(f) = fleet {
        remove_member(world, f, char_id);
    }
}

fn is_leader(f: &Fleet, char_id: i64) -> bool {
    f.members.get(&char_id).is_some_and(|m| m.role == ROLE_LEADER)
}

mod fleet_mgr;
mod fleet_object_handler;

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if let Some(reply) = super::fixed::fleets(call) { return Some(reply); }
    match call.service {
        "fleetObjectHandler" => fleet_object_handler::dispatch(c, call).await,
        "fleetMgr" => fleet_mgr::dispatch(c, call).await,
        _ => None,
    }
}

/// Apply session pairs to the calling client directly.
fn set_own_session(c: &mut Client, pairs: &[Value]) {
    let owned: Vec<(String, Value)> = pairs
        .iter()
        .filter_map(|p| match p.items()? {
            [k, v] => Some((k.as_text()?.into_owned(), v.clone())),
            _ => None,
        })
        .collect();
    c.change_session(owned.iter().map(|(k, v)| (k.as_str(), v.clone())).collect());
}
