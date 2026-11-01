//! Fleet object handler (eve/client/script/parklife/fleetSvc.py).
//! Fleet state is process-local; fleet composition reads character and item data.
//! Advertising, boosts and fleet warps are not modelled.

use super::*;

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let me = c.session.int("charid").unwrap_or(0);
    let int = |i: usize| call.args.get(i).and_then(|v| v.as_int());
    let world = c.world.clone();
    match (call.service, call.method) {
        ("fleetObjectHandler", "CreateFleet") if !call.bound => {
            if c.session.int("fleetid").is_some() {
                return Some(Err(CallError::User { msg: messages::MSG_FLEET_ERROR.into(), dict: vec![] }));
            }
            let id = next_id();
            let (wing, squad) = (next_id(), next_id());
            state().fleets.insert(
                id,
                Fleet {
                    members: HashMap::new(),
                    wings: vec![Wing { id: wing, name: messages::fleets::FLEET_INITIAL_WING_NAME.into(), squads: vec![Squad { id: squad, name: messages::fleets::FLEET_INITIAL_SQUAD_NAME.into() }] }],
                    motd: String::new(),
                    invites: HashMap::new(),
                },
            );
            return Some(Ok(c.bind("fleetObjectHandler", Value::Int(id))));
        }
        _ => {}
    }
    let fleet_id = bound_fleet(call);
    Some(match call.method {
        // Init(shipTypeID, setupName, adInfoData): the creator leads from the first squad.
        "Init" => {
            let member = {
                let mut st = state();
                let Some(f) = st.fleets.get_mut(&fleet_id) else { return Some(Err(notify_error(messages::fleets::FLEET_NOT_FOUND))) };
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
            let Some(f) = st.fleets.get(&fleet_id) else { return Some(Err(notify_error(messages::fleets::FLEET_NOT_FOUND))) };
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
                _ => Err(notify_error(messages::fleets::FLEET_LEADER_REQUIRED_FOR_MOTD)),
            }
        }
        "GetFleetComposition" => {
            let ids: Vec<i64> = state().fleets.get(&fleet_id).map(|f| f.members.keys().copied().collect()).unwrap_or_default();
            match crate::dao::characters::routing::fleet_member_locations(&c.db, &ids).await {
                Ok(rows) => Ok(Value::list(rows.iter().map(|r| {
                    keyval([
                        ("characterID", Value::Int(r.character_id)),
                        ("solarSystemID", r.solar_system_id.map(Value::Int).unwrap_or(Value::None)),
                        ("stationID", r.station_id.map(Value::Int).unwrap_or(Value::None)),
                        ("shipTypeID", r.type_id.map(|t| Value::Int(t as i64)).unwrap_or(Value::None)),
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
                    return Some(Err(notify_error(messages::fleets::PILOT_ALREADY_IN_FLEET)));
                }
                let Some(f) = st.fleets.get_mut(&fleet_id) else { return Some(Err(notify_error(messages::fleets::FLEET_NOT_FOUND))) };
                if !f.members.contains_key(&me) {
                    return Some(Err(notify_error(messages::fleets::PILOT_NOT_IN_FLEET)));
                }
                if f.members.len() >= MAX_MEMBERS {
                    return Some(Err(notify_error(messages::fleets::FLEET_FULL)));
                }
                f.invites.insert(target, (wing, squad, role));
            }
            let name = crate::dao::characters::routing::character_name(&c.db, me).await.ok().flatten().unwrap_or_default();
            let question = messages::fleets::fleet_invitation(&name);
            world.notify_pilot(
                target,
                "OnFleetInvite",
                vec![Value::Int(fleet_id), Value::Int(me), Value::str(messages::MSG_CUSTOM_QUESTION), Value::dict([("header", Value::unicode(messages::fleets::FLEET_INVITATION_HEADER)), ("question", Value::unicode(question))])],
            );
            Ok(Value::None)
        }
        // AcceptInvite(shipTypeID)
        "AcceptInvite" => {
            let joined = {
                let mut st = state();
                if st.of.contains_key(&me) {
                    return Some(Err(notify_error(messages::fleets::PILOT_ALREADY_IN_FLEET_SELF)));
                }
                let Some(f) = st.fleets.get_mut(&fleet_id) else { return Some(Err(notify_error(messages::fleets::FLEET_NOT_FOUND))) };
                let Some((wing, squad, role)) = f.invites.remove(&me) else { return Some(Err(notify_error(messages::fleets::FLEET_INVITATION_EXPIRED))) };
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
                return Some(Err(notify_error(messages::fleets::FLEET_LEADER_REQUIRED_TO_REMOVE_MEMBER)));
            }
            remove_member(&world, fleet_id, target);
            Ok(Value::None)
        }
        "DisbandFleet" => {
            let members: Vec<i64> = match state().fleets.get(&fleet_id) {
                Some(f) if is_leader(f, me) => f.members.keys().copied().collect(),
                _ => return Some(Err(notify_error(messages::fleets::FLEET_LEADER_REQUIRED_TO_DISBAND))),
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
            let Some(f) = st.fleets.get_mut(&fleet_id) else { return Some(Err(notify_error(messages::fleets::FLEET_NOT_FOUND))) };
            let free_move = target == me && call.method == "MoveMember";
            if !is_leader(f, me) && !free_move {
                return Some(Err(notify_error(messages::fleets::FLEET_LEADER_REQUIRED_TO_MOVE_MEMBER)));
            }
            let Some(before) = f.members.get(&target).cloned() else { return Some(Err(notify_error(messages::fleets::PILOT_NOT_IN_FLEET_TARGET))) };
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
                    f.wings.push(Wing { id, name: messages::fleets::fleet_wing_name(n), squads: vec![] });
                    broadcast(&world, f, None, "OnFleetWingAdded", vec![Value::Int(id)]);
                    Ok(Value::Int(id))
                }
                _ => Err(notify_error(messages::fleets::FLEET_LEADER_REQUIRED_TO_ADD_WING)),
            }
        }
        // CreateSquad(wingID)
        "CreateSquad" => {
            let wing = int(0).unwrap_or(0);
            let mut st = state();
            match st.fleets.get_mut(&fleet_id) {
                Some(f) if is_leader(f, me) => {
                    let id = next_id();
                    let Some(w) = f.wings.iter_mut().find(|w| w.id == wing) else { return Some(Err(notify_error(messages::fleets::FLEET_WING_NOT_FOUND))) };
                    let n = w.squads.len() + 1;
                    w.squads.push(Squad { id, name: messages::fleets::fleet_squad_name(n) });
                    broadcast(&world, f, None, "OnFleetSquadAdded", vec![Value::Int(wing), Value::Int(id)]);
                    Ok(Value::Int(id))
                }
                _ => Err(notify_error(messages::fleets::FLEET_LEADER_REQUIRED_TO_ADD_SQUAD)),
            }
        }
        "DeleteWing" | "DeleteSquad" => {
            let id = int(0).unwrap_or(0);
            let mut st = state();
            match st.fleets.get_mut(&fleet_id) {
                Some(f) if is_leader(f, me) => {
                    let occupied = f.members.values().any(|m| m.wing == id || m.squad == id);
                    if occupied {
                        return Some(Err(notify_error(messages::fleets::FLEET_STRUCTURE_MUST_BE_EMPTY)));
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
                _ => Err(notify_error(messages::fleets::FLEET_LEADER_REQUIRED_TO_CHANGE_STRUCTURE)),
            }
        }
        // ChangeWingName(wingID, name) / ChangeSquadName(squadID, name)
        "ChangeWingName" | "ChangeSquadName" => {
            let id = int(0).unwrap_or(0);
            let name = value_text(call.args.get(1), MAX_NAME_LENGTH);
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
                _ => Err(notify_error(messages::fleets::FLEET_LEADER_REQUIRED_TO_RENAME_WING_OR_SQUAD)),
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
        _ => return None,
    })
}
