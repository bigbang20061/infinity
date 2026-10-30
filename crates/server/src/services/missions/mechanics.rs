//! Mission state, offers, rewards and arc mechanics; completion commits before dispatching
//! effects. The client renders dungeon objectives in
//! `agentinteraction/objectivesteps.py:103-104,123-125,137-142`.
//! Mission and dungeon candidates and messages come from runtime `sde_native` tables;
//! placement uses the shared SDE. Mission, character, item, loyalty and standing state is
//! stored in the game database; site simulation is separate.
//! Rewards and placement are server policy, not a client-provided generator; complete
//! live-server mission layouts and mechanics are not reproduced.
use super::*;
/// (ISK, LP) rewards by agent level.
fn rewards(level: i64) -> (i64, i64) {
    match level {
        1 => (90_000, 150),
        2 => (350_000, 450),
        3 => (1_000_000, 1_300),
        4 => (2_800_000, 3_500),
        _ => (7_000_000, 8_000),
    }
}

fn mission_from(r: crate::dao::missions::MissionRow) -> Mission {
    Mission {
        agent_id: r.agent_id,
        mission_id: r.mission_id,
        name_id: r.name_id.unwrap_or(0),
        briefing_id: r.briefing_id.unwrap_or(0),
        dungeon_id: r.dungeon_id,
        faction_id: r.faction_id,
        state: r.state,
        system_id: r.system_id,
        pos: [r.x, r.y, r.z],
        reward_isk: r.reward_isk,
        reward_lp: r.reward_lp,
        accepted_at: r.accepted_at,
        kind: Kind::of_code(&r.kind),
        arc_id: r.arc_id,
        dest_agent: r.dest_agent,
        cargo: r.cargo_type.zip(r.cargo_qty),
    }
}

pub(super) async fn missions_of(c: &Client, agent_id: Option<i64>) -> Result<Vec<Mission>, CallError> {
    let me = c.session.int("charid").unwrap_or(0);
    Ok(crate::dao::missions::missions_of(&c.db, me, agent_id).await.map_err(db_err)?.into_iter().map(mission_from).collect())
}

pub(super) async fn mission_with(c: &Client, agent_id: i64) -> Result<Option<Mission>, CallError> {
    Ok(missions_of(c, Some(agent_id)).await?.into_iter().next())
}

/// A mission message ID of this mission (`messages.mission.accepted.agentsays`...).
pub(super) async fn message(c: &Client, mission_id: i32, key: &str) -> Result<Option<i64>, CallError> {
    Ok(sde_rows::Message::from_row(&c.db
        .query_one("SELECT (data->'messages'->>$2)::bigint AS message_id FROM sde_native.missions WHERE key = $1::int::text", &[&mission_id, &key])
        .await
        .map_err(db_err)?).id)
}

/// A mission site: a point well off the surface of a random planet of the system.
fn mission_site_position(c: &Client, system: i64) -> V3 {
    let (base, radius) = super::site_position(&c.sde, system).unwrap_or(([0.0; 3], 0.0));
    super::off_surface(base, radius, SITE_OFFSET)
}

/// Offer one of the build's kill missions at the agent's level, sited in the agent's system.
pub(super) async fn offer(c: &Client, agent: &Agent) -> Result<(), CallError> {
    let me = c.session.int("charid").unwrap_or(0);
    let level = agent.level.clamp(1, 5) as i32;
    let pick = c
        .db
        .query_opt(
            "SELECT m.key::int AS mission_id, (m.data->'killMission'->>'dungeonID')::int AS dungeon_id, COALESCE((d.data->>'factionID')::bigint, 0) AS faction_id
             FROM sde_native.missions m JOIN sde_native.dungeons d ON d.key = m.data->'killMission'->>'dungeonID'
             WHERE m.data->>'contentTemplate' = 'agent.missionTemplatizedContent_BasicKillMission'
               AND (d.data->>'difficulty')::int = $1 AND m.data ? 'nameID' AND m.data->'messages' ? 'messages.mission.briefing'
             ORDER BY random() LIMIT 1",
            &[&level],
        )
        .await
        .map_err(db_err)?
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("no level {level} kill missions")))?;
    let pick = sde_rows::Pick::from_row(&pick);
    let pos = mission_site_position(c, agent.system_id);
    let (isk, lp) = rewards(agent.level);
    crate::dao::missions::offer(&c.db, &crate::dao::missions::Offer {
        character_id: me, agent_id: agent.id, mission_id: pick.mission_id, dungeon_id: pick.dungeon_id,
        faction_id: pick.faction_id, state: STATE_OFFERED, system_id: agent.system_id, pos,
        reward_isk: isk, reward_lp: lp, offered_at: filetime_now(),
    }).await.map_err(db_err)?;
    info!(user = c.login.user_id, "agent {}: offered mission {} (dungeon {})", agent.id, pick.mission_id, pick.dungeon_id);
    Ok(())
}

// ---------------------------------------------------------------- epic arcs

/// One step of an epic arc.
pub(super) struct ArcStep {
    arc: i64,
    agent: i64,
    next: Vec<i32>,
    pub(super) fail: Option<i32>,
    /// The arc begins here (no step leads to it).
    start: bool,
}

/// Every arc step by mission ID (sde_native.epicarcs), loaded once.
pub(super) async fn arcs(c: &Client) -> Result<&'static HashMap<i32, ArcStep>, CallError> {
    static ARCS: OnceLock<HashMap<i32, ArcStep>> = OnceLock::new();
    if let Some(a) = ARCS.get() {
        return Ok(a);
    }
    let rows = c
        .db
        .query(
            "SELECT e.key::bigint AS arc_id, m.key::int AS mission_id, (m.value->>'agentID')::bigint AS agent_id, COALESCE(m.value->'nextMissions', '[]'::jsonb)::text AS next_missions,
                    (m.value->>'failMissionID')::int AS fail_mission_id
             FROM sde_native.epicarcs e, jsonb_each(e.data->'missions') m",
            &[],
        )
        .await
        .map_err(db_err)?;
    let mut steps: HashMap<i32, ArcStep> = HashMap::new();
    for r in &rows {
        let r = sde_rows::ArcStepRow::from_row(r);
        let next: Vec<i32> = r.next.trim_matches(|c| c == '[' || c == ']').split(',').filter_map(|n| n.trim().parse().ok()).collect();
        steps.insert(r.mission_id, ArcStep { arc: r.arc_id, agent: r.agent_id.unwrap_or(0), next, fail: r.fail, start: true });
    }
    let followers: HashSet<i32> = steps.values().flat_map(|s| s.next.iter().copied()).collect();
    for (id, s) in steps.iter_mut() {
        s.start = !followers.contains(id);
    }
    Ok(ARCS.get_or_init(|| steps))
}

pub(super) async fn is_arc_agent(c: &Client, agent_id: i64) -> Result<bool, CallError> {
    Ok(arcs(c).await?.values().any(|s| s.agent == agent_id))
}

/// The arc step this agent has for us next: the start of an arc we haven't begun, or a
/// step our progress has unlocked.
pub(super) async fn next_arc_step(c: &Client, agent_id: i64) -> Result<Option<i32>, CallError> {
    let me = c.session.int("charid").unwrap_or(0);
    let rows = crate::dao::missions::arc_progress(&c.db, me).await.map_err(db_err)?;
    let begun: HashSet<i64> = rows.iter().map(|r| r.arc_id).collect();
    let unlocked: HashSet<i32> = rows.iter().filter(|r| r.state == "unlocked").map(|r| r.mission_id).collect();
    let mut mine: Vec<(&i32, &ArcStep)> = arcs(c).await?.iter().filter(|(_, s)| s.agent == agent_id).collect();
    mine.sort_by_key(|(id, _)| **id);
    Ok(mine.into_iter().find(|(id, s)| unlocked.contains(id) || (s.start && !begun.contains(&s.arc))).map(|(id, _)| *id))
}

pub(super) async fn set_arc_state(c: &Client, arc: i64, mission: i32, state: &str) -> Result<(), CallError> {
    let me = c.session.int("charid").unwrap_or(0);
    let stamp = if state == "unlocked" { None } else { Some(filetime_now()) };
    crate::dao::missions::set_arc_state(&c.db, me, arc, mission, state, stamp).await.map_err(db_err)?;
    Ok(())
}

/// Offer an epic arc step: its content (kill site, courier cargo, trade goods...) and
/// rewards come from the mission's own data.
pub(super) async fn offer_arc(c: &Client, agent: &Agent, mission_id: i32) -> Result<(), CallError> {
    let me = c.session.int("charid").unwrap_or(0);
    let steps = arcs(c).await?;
    let Some(step) = steps.get(&mission_id) else { return Ok(()) };
    let r = c
        .db
        .query_one(
            "SELECT m.data->>'contentTemplate' AS template, COALESCE((m.data->'killMission'->>'dungeonID')::int, 0) AS dungeon_id,
                    COALESCE((d.data->>'factionID')::bigint, (m.data->>'factionID')::bigint, 0) AS faction_id,
                    (m.data->'courierMission'->>'objectiveTypeID')::int AS cargo_type, (m.data->'courierMission'->>'objectiveQuantity')::int AS cargo_qty,
                    CASE WHEN (m.data->'missionRewards'->'reward'->>'rewardTypeID')::int = $2
                         THEN (m.data->'missionRewards'->'reward'->>'rewardQuantity')::bigint ELSE 0 END AS reward_isk,
                    COALESCE((m.data->>'fixedLpRewardOmega')::bigint, 0) AS reward_lp
             FROM sde_native.missions m LEFT JOIN sde_native.dungeons d ON d.key = m.data->'killMission'->>'dungeonID'
             WHERE m.key = $1::text",
            &[&mission_id.to_string(), &(TYPE_CREDITS as i32)],
        )
        .await
        .map_err(db_err)?;
    let r = sde_rows::ArcOfferRow::from_row(&r);
    let kind = Kind::of_template(&r.template.unwrap_or_default());
    // Talk-to-agent and courier steps end at the agent of the step that follows.
    let dest = match kind {
        Kind::Talk | Kind::Courier => step.next.first().and_then(|n| steps.get(n)).map(|s| s.agent).filter(|a| *a != agent.id),
        _ => None,
    };
    let cargo_type = r.cargo_type;
    let cargo_qty: Option<i32> = cargo_type.map(|_| r.cargo_qty.unwrap_or(1).max(1));
    let pos = mission_site_position(c, agent.system_id);
    crate::dao::missions::offer_arc(&c.db, &crate::dao::missions::ArcOffer {
        offer: crate::dao::missions::Offer {
            character_id: me, agent_id: agent.id, mission_id, dungeon_id: r.dungeon_id,
            faction_id: r.faction_id, state: STATE_OFFERED, system_id: agent.system_id, pos,
            reward_isk: r.reward_isk, reward_lp: r.reward_lp, offered_at: filetime_now(),
        },
        kind: kind.code(), arc_id: step.arc, dest_agent: dest, cargo_type, cargo_qty,
    }).await.map_err(db_err)?;
    info!(user = c.login.user_id, "agent {}: offered epic arc {} step {mission_id} ({kind:?})", agent.id, step.arc);
    Ok(())
}

/// agentMgr.GetMyEpicArcStatus() -> {arcID: {missionID: (acceptedDate, completedDate,
/// quitDate, nameID)}} (epicArcController._GetMissionStatus).
pub(super) async fn epic_arc_status(c: &Client) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let rows = crate::dao::missions::arc_status(&c.db, me).await.map_err(db_err)?;
    let mut by_arc: Vec<(i64, Vec<(Value, Value)>)> = vec![];
    for r in rows {
        let arc = r.arc_id;
        let stamp = |v: Option<i64>| v.map(Value::Int).unwrap_or(Value::None);
        let status = keyval([("acceptedDate", stamp(r.accepted_at)), ("completedDate", stamp(r.completed_at)), ("quitDate", stamp(r.quit_at)), ("nameID", stamp(r.name_id))]);
        match by_arc.last_mut() {
            Some((a, list)) if *a == arc => list.push((Value::Int(r.mission_id as i64), status)),
            _ => by_arc.push((arc, vec![(Value::Int(r.mission_id as i64), status)])),
        }
    }
    Ok(Value::Dict(by_arc.into_iter().map(|(arc, list)| (Value::Int(arc), Value::Dict(list))).collect()))
}

// ---------------------------------------------------------------- cargo

/// Where a pilot can hand goods in at `station`: its hangar there, or its ship's cargo
/// while docked there.
async fn goods_at(c: &Client, station: i64, type_id: i32) -> Result<Vec<crate::dao::items::loading::Item>, CallError> {
    let me = c.session.int("charid").unwrap_or(0);
    let ship = if c.session.int("stationid") == Some(station) { c.session.int("shipid").unwrap_or(0) } else { 0 };
    crate::dao::items::loading::load_items(&c.db, crate::dao::items::loading::ItemQuery::OwnerTypeTwoLocations(me, type_id, station, FLAG_HANGAR, ship, FLAG_CARGO)).await.map_err(db_err)
}

pub(super) async fn has_goods(c: &Client, station: i64, (type_id, qty): (i32, i32)) -> Result<bool, CallError> {
    Ok(goods_at(c, station, type_id).await?.iter().map(|i| i.quantity as i64).sum::<i64>() >= qty as i64)
}

/// Take delivered goods using guarded item changes, retaining inventory notification order.
async fn take_goods<X: Send>(db: &(impl crate::db::executor::Executor + ?Sized), outbox: &mut crate::db::outbox::Outbox<X>, me: i64, station: i64, ship: i64, (type_id, qty): (i32, i32)) -> Result<(), CallError> {
    use crate::dao::items::{loading::{load_items, ItemQuery}, quantities::{self, QuantityChange}};
    let items = load_items(db, ItemQuery::OwnerTypeTwoLocations(me, type_id, station, FLAG_HANGAR, ship, FLAG_CARGO)).await.map_err(db_err)?;
    let mut left = qty;
    let mut changes = std::collections::BTreeMap::new();
    for item in &items {
        if left <= 0 { break; }
        let take = item.quantity.min(left);
        left -= take;
        changes.insert(item.item_id, QuantityChange::new(item, (item.quantity > take).then_some(item.quantity - take)));
    }
    if left > 0 || !quantities::apply(db, &changes).await.map_err(db_err)? {
        return Err(super::super::notify_error(messages::MISSION_DELIVER_TO_STATION_HANGAR));
    }
    for item in items {
        if let Some(change) = changes.get(&item.item_id) {
            if change.quantity.is_none() {
                super::inventory::notify_item_removed(outbox, item);
            } else {
                super::inventory::notify_item_change(db, outbox, item.item_id, vec![(IX_STACK_SIZE, Value::Int(item.quantity as i64))]).await?;
            }
        }
    }
    Ok(())
}

/// Can mission `m` be handed in to `agent` now?
pub(super) async fn ready(c: &Client, m: &Mission, agent: &Agent) -> Result<bool, CallError> {
    if m.state != STATE_ACCEPTED || m.hand_in_agent() != agent.id {
        return Ok(false);
    }
    Ok(match m.kind {
        Kind::Kill => objective_done(c, m).await,
        Kind::Talk => true,
        Kind::Courier | Kind::Trade => match m.cargo {
            Some(cargo) => has_goods(c, agent.station_id, cargo).await?,
            None => true,
        },
        Kind::Interaction => false,
    })
}

/// Close a mission: take delivered goods, pay its rewards (ISK, LP-less items into the
/// hangar at `agent`'s station) and, for an arc step, unlock what follows (`choice`: the
/// branch an agent interaction picked).
pub(super) async fn finish(c: &mut Client, m: &Mission, agent: &Agent, choice: Option<usize>) -> Result<(), CallError> {
    let me = c.session.int("charid").unwrap_or(0);
    let ship = if c.session.int("stationid") == Some(agent.station_id) { c.session.int("shipid").unwrap_or(0) } else { 0 };
    let cargo = if matches!(m.kind, Kind::Courier | Kind::Trade) { m.cargo } else { None };
    let mut reward = None;
    let mut unlocked = Vec::new();
    if m.arc_id.is_some() {
        let row = c.db.query_opt(
            "SELECT (data->'missionRewards'->'reward'->>'rewardTypeID')::int AS type_id, (data->'missionRewards'->'reward'->>'rewardQuantity')::int AS quantity
                 FROM sde_native.missions WHERE key = $1::text AND (data->'missionRewards'->'reward'->>'rewardTypeID')::int <> $2",
            &[&m.mission_id.to_string(), &(TYPE_CREDITS as i32)],
        ).await.map_err(db_err)?;
        if let Some(row) = row {
            let r = sde_rows::Reward::from_row(&row);
            reward = r.type_id.zip(r.quantity).map(|(type_id, qty)| (type_id, qty.max(1)));
        }
        let next = arcs(c).await?.get(&m.mission_id).map(|s| s.next.clone()).unwrap_or_default();
        unlocked = match choice {
            Some(k) => next.get(k).or(next.last()).copied().into_iter().collect(),
            None => next,
        };
    }
    let mission_id = m.mission_id;
    let mission_agent = m.agent_id;
    let arc = m.arc_id;
    let offered_interaction = m.state == STATE_OFFERED && m.kind == Kind::Interaction;
    let reward_isk = m.reward_isk;
    let reward_lp = m.reward_lp;
    let (agent_id, corp, faction, level, station) = (agent.id, agent.corp_id, agent.faction_id, agent.level, agent.station_id);
    let stamp = filetime_now();
    let arc_states = arc.map(|_| {
        let mut states = vec![(mission_id, "completed", Some(stamp))];
        states.extend(unlocked.into_iter().map(|id| (id, "unlocked", None)));
        states.sort_by_key(|row| row.0);
        std::sync::Arc::new(states)
    });
    let _lock = super::super::market::lock().await;
    let db = c.db.clone();
    db.transaction(
        move |tx, outbox: &mut crate::db::outbox::Outbox<()>| {
            let arc_states = arc_states.clone();
            Box::pin(async move {
                if let Some(cargo) = cargo {
                    take_goods(tx, outbox, me, station, ship, cargo).await?;
                }
                if offered_interaction && crate::dao::missions::accept_offered(tx, me, mission_agent, mission_id, STATE_OFFERED, STATE_ACCEPTED, stamp).await.map_err(db_err)? == 0 {
                    return Err(super::super::notify_error(messages::MISSION_DELIVER_TO_STATION_HANGAR));
                }
                if crate::dao::missions::remove_accepted(tx, me, mission_agent, mission_id, STATE_ACCEPTED).await.map_err(db_err)? == 0 {
                    return Err(super::super::notify_error(messages::MISSION_DELIVER_TO_STATION_HANGAR));
                }
                outbox.operation(());
                if reward_isk > 0 {
                    let balance = crate::dao::wallets::characters::credit(tx, me, reward_isk as f64).await.map_err(db_err)?;
                    outbox.notify_client("OnAccountChange", vec![Value::str("cash"), Value::Int(me), Value::Float(balance)]);
                }
                super::standings::add_lp(tx, outbox, me, corp, reward_lp).await?;
                super::standings::mission_completed(tx, outbox, me, agent_id, corp, faction, level).await?;
                if let Some(arc) = arc {
                    if let Some((type_id, qty)) = reward {
                        // Deposit selects its target stack itself, which may sort below consumed
                        // stacks. The economy lock serializes economy operations; the transaction
                        // runner retries deadlocks once.
                        super::items::deposit(tx, outbox, me, station, FLAG_HANGAR, type_id, qty, 0).await?;
                    }
                    for &(id, state, state_stamp) in arc_states.as_ref().unwrap().iter() {
                        if offered_interaction && id == mission_id && state == "completed" {
                            crate::dao::missions::set_arc_state(tx, me, arc, id, "accepted", Some(stamp)).await.map_err(db_err)?;
                        }
                        crate::dao::missions::set_arc_state(tx, me, arc, id, state, state_stamp).await.map_err(db_err)?;
                    }
                }
                outbox.notify_client("OnAgentMissionChange", vec![Value::str("completed"), Value::Int(mission_agent)]);
                Ok(())
            })
        },
        |(), outbox| {
            let client = &mut *c;
            async move {
                let world = client.world.clone();
                for entry in outbox.into_entries() {
                    match entry {
                        crate::db::outbox::Effect::Notification(notification) => notification.dispatch(&world, Some(&mut *client))?,
                        crate::db::outbox::Effect::Operation(()) => despawn(client, me, mission_agent),
                    }
                }
                Ok(())
            }
        },
    ).await?;
    info!(user = c.login.user_id, "agent {}: mission {} completed at {}, {} ISK", mission_agent, mission_id, agent_id, reward_isk);
    Ok(())
}

/// An agent interaction's answers: messages.mission.option<N>.charsays.
pub(super) async fn options(c: &Client, mission_id: i32) -> Result<Vec<i64>, CallError> {
    let mut out = vec![];
    for n in 1..=4 {
        match message(c, mission_id, &format!("messages.mission.option{n}.charsays")).await? {
            Some(id) => out.push(id),
            None => break,
        }
    }
    Ok(out)
}

pub(super) async fn set_state(c: &Client, agent_id: i64, state: i32) -> Result<(), CallError> {
    let me = c.session.int("charid").unwrap_or(0);
    crate::dao::missions::set_state(&c.db, me, agent_id, state, filetime_now()).await.map_err(db_err)?;
    Ok(())
}

pub(super) async fn remove(c: &Client, agent_id: i64) -> Result<(), CallError> {
    let me = c.session.int("charid").unwrap_or(0);
    crate::dao::missions::remove(&c.db, me, agent_id).await.map_err(db_err)?;
    despawn(c, me, agent_id);
    Ok(())
}

