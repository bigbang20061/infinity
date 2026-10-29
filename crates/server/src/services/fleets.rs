//! Fleets (fleetObjectHandler, bound per fleet; eve/client/script/parklife/fleetSvc.py).
//!
//! A fleet lives in memory while it has members: wings of squads, a leader, a MOTD and the
//! invitations out. Creating one (`CreateFleet` then `Init`) makes the creator its leader in
//! its first squad; an invited pilot accepts through their own bound fleet object. Members
//! carry the fleet in their session (fleetid, wingid, squadid, fleetrole), which also puts
//! them in the fleet chat channel; joins, departures and changes are pushed to every member
//! (OnFleetJoin, OnFleetLeave, OnFleetMemberChanged...). A pilot who logs off leaves.
//! Fleet warps, boosts, ads and the fleet finder aren't supported.

use super::{keyval, Call, CallError, CallResult};
use crate::client::Client;
use crate::space::World;
use marshal::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use tracing::info;

// evefleet.const
const ROLE_LEADER: i64 = 1;
const ROLE_MEMBER: i64 = 4;
const JOB_NONE: i64 = 0;
const JOB_CREATOR: i64 = 2;
const NONE_ID: i64 = -1;
const MAX_MEMBERS: usize = 256;
const MAX_NAME_LENGTH: usize = 20;

static NEXT_ID: AtomicI64 = AtomicI64::new(1);

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

fn next_id() -> i64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

fn notify_error(text: &str) -> CallError {
    CallError::User { msg: "CustomNotify".into(), dict: vec![("notify".into(), Value::unicode(text))] }
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

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let me = c.session.int("charid").unwrap_or(0);
    let int = |i: usize| call.args.get(i).and_then(|v| v.as_int());
    let world = c.world.clone();
    match (call.service, call.method) {
        // {fleetID: advert dict} for the fleet finder: fleets don't advertise here.
        ("fleetProxy", "GetAvailableFleetAds") => return Some(Ok(Value::Dict(vec![]))),
        ("fleetObjectHandler", "CreateFleet") if !call.bound => {
            if c.session.int("fleetid").is_some() {
                return Some(Err(CallError::User { msg: "FleetError".into(), dict: vec![] }));
            }
            let id = next_id();
            let (wing, squad) = (next_id(), next_id());
            state().fleets.insert(
                id,
                Fleet {
                    members: HashMap::new(),
                    wings: vec![Wing { id: wing, name: "联队 1".into(), squads: vec![Squad { id: squad, name: "中队 1".into() }] }],
                    motd: String::new(),
                    invites: HashMap::new(),
                },
            );
            return Some(Ok(c.bind("fleetObjectHandler", Value::Int(id))));
        }
        ("fleetMgr", "ForceLeaveFleet") => {
            logged_off(&world, me);
            return Some(Ok(Value::None));
        }
        ("fleetMgr", "AddToWatchlist" | "RemoveFromWatchlist" | "RegisterForDamageUpdates" | "BroadcastToSystem" | "BroadcastToBubble") => return Some(Ok(Value::None)),
        ("fleetObjectHandler", _) => {}
        _ => return None,
    }
    let fleet_id = bound_fleet(call);
    Some(match call.method {
        // Init(shipTypeID, setupName, adInfoData): the creator leads from the first squad.
        "Init" => {
            let member = {
                let mut st = state();
                let Some(f) = st.fleets.get_mut(&fleet_id) else { return Some(Err(notify_error("该舰队已不存在。"))) };
                let squad = f.wings.first().and_then(|w| w.squads.first().map(|s| (w.id, s.id))).unwrap_or((NONE_ID, NONE_ID));
                let m = Member { wing: squad.0, squad: squad.1, role: ROLE_LEADER, job: JOB_CREATOR, ship_type: int(0).unwrap_or(0) };
                f.members.insert(me, m.clone());
                st.of.insert(me, fleet_id);
                m
            };
            let pairs = session_pairs(Some(fleet_id), Some(&member));
            set_own_session(c, &pairs);
            info!("fleets: {me} formed fleet {fleet_id}");
            Ok(Value::None)
        }
        "GetFleetID" => Ok(Value::Int(fleet_id)),
        "GetInitState" => {
            let st = state();
            let Some(f) = st.fleets.get(&fleet_id) else { return Some(Err(notify_error("该舰队已不存在。"))) };
            Ok(keyval([
                ("fleetID", Value::Int(fleet_id)),
                ("members", Value::Dict(f.members.iter().map(|(ch, m)| (Value::Int(*ch), member_value(*ch, m))).collect())),
                ("wings", wings_value(f)),
                (
                    "options",
                    keyval([("isFreeMove", Value::Bool(false)), ("isRegistered", Value::Bool(false)), ("isVoiceEnabled", Value::Bool(false)), ("autoJoinSquadID", Value::None)]),
                ),
                ("motd", Value::unicode(f.motd.as_str())),
            ]))
        }
        "GetWings" => match state().fleets.get(&fleet_id) {
            Some(f) => Ok(wings_value(f)),
            None => Ok(Value::Dict(vec![])),
        },
        "GetMotd" => Ok(Value::unicode(state().fleets.get(&fleet_id).map(|f| f.motd.clone()).unwrap_or_default())),
        "SetMotdEx" => {
            let motd = call.args.first().and_then(|v| v.as_text()).map(|t| t.into_owned()).unwrap_or_default();
            let mut st = state();
            match st.fleets.get_mut(&fleet_id) {
                Some(f) if is_leader(f, me) => {
                    f.motd = motd.clone();
                    broadcast(&world, f, None, "OnFleetMotdChanged", vec![Value::unicode(motd), Value::Bool(true)]);
                    Ok(Value::None)
                }
                _ => Err(notify_error("只有舰队指挥官才能设置每日消息。")),
            }
        }
        "GetFleetMaxSize" => Ok(Value::Int(MAX_MEMBERS as i64)),
        "GetJoinRequests" => Ok(Value::Dict(vec![])),
        "GetActiveBridgeForShip" => Ok(Value::None),
        "GetFleetComposition" => {
            let ids: Vec<i64> = state().fleets.get(&fleet_id).map(|f| f.members.keys().copied().collect()).unwrap_or_default();
            match c.db.client.query("SELECT ch.character_id, ch.solar_system_id, ch.station_id, i.type_id FROM game.characters ch LEFT JOIN game.items i ON i.item_id = ch.ship_id WHERE ch.character_id = ANY($1)", &[&ids]).await {
                Ok(rows) => Ok(Value::list(rows.iter().map(|r| {
                    keyval([
                        ("characterID", Value::Int(r.get(0))),
                        ("solarSystemID", r.get::<_, Option<i64>>(1).map(Value::Int).unwrap_or(Value::None)),
                        ("stationID", r.get::<_, Option<i64>>(2).map(Value::Int).unwrap_or(Value::None)),
                        ("shipTypeID", r.get::<_, Option<i32>>(3).map(|t| Value::Int(t as i64)).unwrap_or(Value::None)),
                        ("skills", Value::list([])),
                        ("skillIDs", Value::list([])),
                    ])
                }))),
                Err(e) => Err(CallError::Internal(e.into())),
            }
        }
        // Invite(charID, wingID, squadID, role)
        "Invite" => {
            let target = int(0).unwrap_or(0);
            let (wing, squad, role) = (int(1).unwrap_or(NONE_ID), int(2).unwrap_or(NONE_ID), int(3).unwrap_or(ROLE_MEMBER));
            {
                let mut st = state();
                if st.of.contains_key(&target) {
                    return Some(Err(notify_error("该飞行员已在舰队中。")));
                }
                let Some(f) = st.fleets.get_mut(&fleet_id) else { return Some(Err(notify_error("该舰队已不存在。"))) };
                if !f.members.contains_key(&me) {
                    return Some(Err(notify_error("你不在该舰队中。")));
                }
                if f.members.len() >= MAX_MEMBERS {
                    return Some(Err(notify_error("舰队已满。")));
                }
                f.invites.insert(target, (wing, squad, role));
            }
            let name: String = c
                .db
                .client
                .query_opt("SELECT name FROM game.characters WHERE character_id = $1", &[&me])
                .await
                .ok()
                .flatten()
                .map(|r| r.get(0))
                .unwrap_or_default();
            let question = format!("{name}邀请你加入舰队。是否接受？");
            world.notify_pilot(
                target,
                "OnFleetInvite",
                vec![Value::Int(fleet_id), Value::Int(me), Value::str("CustomQuestion"), Value::dict([("header", Value::unicode("舰队邀请")), ("question", Value::unicode(question))])],
            );
            Ok(Value::None)
        }
        // AcceptInvite(shipTypeID)
        "AcceptInvite" => {
            let joined = {
                let mut st = state();
                if st.of.contains_key(&me) {
                    return Some(Err(notify_error("你已在舰队中。")));
                }
                let Some(f) = st.fleets.get_mut(&fleet_id) else { return Some(Err(notify_error("该舰队已不存在。"))) };
                let Some((wing, squad, role)) = f.invites.remove(&me) else { return Some(Err(notify_error("该邀请已过期。"))) };
                let fallback = f.wings.first().and_then(|w| w.squads.first().map(|s| (w.id, s.id))).unwrap_or((NONE_ID, NONE_ID));
                let valid = f.wings.iter().any(|w| w.id == wing && (squad == NONE_ID || w.squads.iter().any(|s| s.id == squad)));
                let (wing, squad) = if valid { (wing, squad) } else { fallback };
                let role = if role == ROLE_LEADER { ROLE_MEMBER } else { role };
                let m = Member { wing, squad, role, job: JOB_NONE, ship_type: int(0).unwrap_or(0) };
                f.members.insert(me, m.clone());
                broadcast(&world, f, Some(me), "OnFleetJoin", vec![member_value(me, &m)]);
                st.of.insert(me, fleet_id);
                m
            };
            let pairs = session_pairs(Some(fleet_id), Some(&joined));
            set_own_session(c, &pairs);
            info!("fleets: {me} joined fleet {fleet_id}");
            Ok(Value::None)
        }
        // RejectInvite(busy)
        "RejectInvite" => {
            let mut st = state();
            if let Some(f) = st.fleets.get_mut(&fleet_id) {
                if f.invites.remove(&me).is_some() {
                    broadcast(&world, f, None, "OnFleetJoinReject", vec![Value::Int(me), Value::None]);
                }
            }
            Ok(Value::None)
        }
        "LeaveFleet" => {
            remove_member(&world, fleet_id, me);
            Ok(Value::None)
        }
        "KickMember" => {
            let target = int(0).unwrap_or(0);
            let allowed = state().fleets.get(&fleet_id).is_some_and(|f| is_leader(f, me) && f.members.contains_key(&target));
            if !allowed {
                return Some(Err(notify_error("只有舰队指挥官才能移除成员。")));
            }
            remove_member(&world, fleet_id, target);
            Ok(Value::None)
        }
        "DisbandFleet" => {
            let members: Vec<i64> = match state().fleets.get(&fleet_id) {
                Some(f) if is_leader(f, me) => f.members.keys().copied().collect(),
                _ => return Some(Err(notify_error("只有舰队指挥官才能解散舰队。"))),
            };
            for ch in members {
                remove_member(&world, fleet_id, ch);
            }
            Ok(Value::None)
        }
        // MakeLeader(charID) / MoveMember(charID, wingID, squadID, role)
        "MakeLeader" | "MoveMember" => {
            let target = int(0).unwrap_or(0);
            let mut st = state();
            let Some(f) = st.fleets.get_mut(&fleet_id) else { return Some(Err(notify_error("该舰队已不存在。"))) };
            let free_move = target == me && call.method == "MoveMember";
            if !is_leader(f, me) && !free_move {
                return Some(Err(notify_error("只有舰队指挥官才能调动成员。")));
            }
            let Some(before) = f.members.get(&target).cloned() else { return Some(Err(notify_error("该飞行员不在舰队中。"))) };
            let mut after = before.clone();
            if call.method == "MakeLeader" {
                after.role = ROLE_LEADER;
            } else {
                after.wing = int(1).unwrap_or(before.wing);
                after.squad = int(2).unwrap_or(before.squad);
                after.role = int(3).unwrap_or(before.role).clamp(ROLE_LEADER, ROLE_MEMBER);
            }
            let mut changes = vec![(target, before, after.clone())];
            // There is one leader; the old one becomes a member.
            if after.role == ROLE_LEADER {
                for (ch, m) in f.members.iter_mut().filter(|(ch, m)| **ch != target && m.role == ROLE_LEADER) {
                    let old = m.clone();
                    m.role = ROLE_MEMBER;
                    changes.push((*ch, old, m.clone()));
                }
            }
            if let Some(m) = f.members.get_mut(&target) {
                *m = after;
            }
            let only = f.members.len() == 1;
            for (ch, before, after) in changes {
                world.notify_pilot(ch, "__session", session_pairs(Some(fleet_id), Some(&after)));
                broadcast(&world, f, None, "OnFleetMemberChanged", changed(ch, fleet_id, &before, &after, only));
            }
            Ok(Value::None)
        }
        "CreateWing" => {
            let mut st = state();
            match st.fleets.get_mut(&fleet_id) {
                Some(f) if is_leader(f, me) => {
                    let id = next_id();
                    let n = f.wings.len() + 1;
                    f.wings.push(Wing { id, name: format!("联队 {n}"), squads: vec![] });
                    broadcast(&world, f, None, "OnFleetWingAdded", vec![Value::Int(id)]);
                    Ok(Value::Int(id))
                }
                _ => Err(notify_error("只有舰队指挥官才能添加联队。")),
            }
        }
        // CreateSquad(wingID)
        "CreateSquad" => {
            let wing = int(0).unwrap_or(0);
            let mut st = state();
            match st.fleets.get_mut(&fleet_id) {
                Some(f) if is_leader(f, me) => {
                    let id = next_id();
                    let Some(w) = f.wings.iter_mut().find(|w| w.id == wing) else { return Some(Err(notify_error("该联队已不存在。"))) };
                    let n = w.squads.len() + 1;
                    w.squads.push(Squad { id, name: format!("中队 {n}") });
                    broadcast(&world, f, None, "OnFleetSquadAdded", vec![Value::Int(wing), Value::Int(id)]);
                    Ok(Value::Int(id))
                }
                _ => Err(notify_error("只有舰队指挥官才能添加中队。")),
            }
        }
        "DeleteWing" | "DeleteSquad" => {
            let id = int(0).unwrap_or(0);
            let mut st = state();
            match st.fleets.get_mut(&fleet_id) {
                Some(f) if is_leader(f, me) => {
                    let occupied = f.members.values().any(|m| m.wing == id || m.squad == id);
                    if occupied {
                        return Some(Err(notify_error("请先将其中的成员移出。")));
                    }
                    if call.method == "DeleteWing" {
                        f.wings.retain(|w| w.id != id);
                        broadcast(&world, f, None, "OnFleetWingDeleted", vec![Value::Int(id)]);
                    } else {
                        for w in &mut f.wings {
                            w.squads.retain(|s| s.id != id);
                        }
                        broadcast(&world, f, None, "OnFleetSquadDeleted", vec![Value::Int(id)]);
                    }
                    Ok(Value::None)
                }
                _ => Err(notify_error("只有舰队指挥官才能调整舰队结构。")),
            }
        }
        // ChangeWingName(wingID, name) / ChangeSquadName(squadID, name)
        "ChangeWingName" | "ChangeSquadName" => {
            let id = int(0).unwrap_or(0);
            let name: String = call.args.get(1).and_then(|v| v.as_text()).map(|t| t.chars().take(MAX_NAME_LENGTH).collect()).unwrap_or_default();
            let mut st = state();
            match st.fleets.get_mut(&fleet_id) {
                Some(f) if is_leader(f, me) => {
                    let event = if call.method == "ChangeWingName" {
                        for w in f.wings.iter_mut().filter(|w| w.id == id) {
                            w.name = name.clone();
                        }
                        "OnFleetWingNameChanged"
                    } else {
                        for s in f.wings.iter_mut().flat_map(|w| w.squads.iter_mut()).filter(|s| s.id == id) {
                            s.name = name.clone();
                        }
                        "OnFleetSquadNameChanged"
                    };
                    broadcast(&world, f, None, event, vec![Value::Int(id), Value::unicode(name)]);
                    Ok(Value::None)
                }
                _ => Err(notify_error("只有舰队指挥官才能重命名联队和中队。")),
            }
        }
        // SendBroadcast(name, scope, itemID, typeID)
        "SendBroadcast" => {
            let name = call.args.first().cloned().unwrap_or(Value::None);
            let scope = call.args.get(1).cloned().unwrap_or(Value::None);
            let item = call.args.get(2).cloned().unwrap_or(Value::None);
            let type_id = call.args.get(3).cloned().unwrap_or(Value::None);
            let system = c.session.int("solarsystemid2").map(Value::Int).unwrap_or(Value::None);
            if let Some(f) = state().fleets.get(&fleet_id) {
                broadcast(&world, f, None, "OnFleetBroadcast", vec![name, scope, Value::Int(me), system, item, type_id]);
            }
            Ok(Value::None)
        }
        "UpdateMemberInfo" | "SetOptions" | "SetAutoJoinSquadID" | "SetTakesFleetWarp" | "SetAcceptsRegroupValue" | "SetAcceptsConduitJumpsValue" | "SetFleetMaxSize"
        | "FinishMove" | "LoadFleetSetup" | "RejectJoinRequest" | "MassMoveMembers" | "ArrangeMembersByShipType" | "ArrangeMembersByPreCount"
        | "ArrangeMembersByEvenlyDistribute" | "ArrangeMembersByAllToFirst" => Ok(Value::None),
        _ => return None,
    })
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
