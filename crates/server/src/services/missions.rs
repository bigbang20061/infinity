//! Agents and kill missions (eve/client/script/ui/station/agents).
//!
//! The client binds one `agentMgr` moniker per agent (eveMoniker.GetAgent) and talks
//! through `DoAction(actionID) -> ((agentSays, [(actionID, button)]), lastActionInfo)`,
//! then renders the briefing (GetMissionBriefingInfo), objectives
//! (GetMissionObjectiveInfo) and journal bookmarks (GetMyJournalDetails).
//!
//! Agents come from the retail agent list (crate::retail). Basic agents offer the
//! build's own kill missions (sde_native.missions, BasicKillMission) whose dungeon
//! difficulty matches the agent's level.
//!
//! Epic arcs (sde_native.epicarcs) chain missions across agents: an arc's first step is
//! offered by its agent to anyone who hasn't begun it, and each completed step unlocks
//! the next ones (an agent-interaction step unlocks the branch the pilot chose). Steps are
//! kill missions, talk-to-agent and courier missions (finished at the next step's agent;
//! courier cargo goes to the pilot's hangar on accepting), trade missions (bring the
//! goods back) and agent interactions (a choice of answers). The live server's dungeon layouts never
//! shipped with the client, so a site is a deadspace point near a planet of the
//! agent's system, spawned with that dungeon faction's mission NPCs on first approach.
//! The objective completes once every spawned NPC is gone.

use super::{keyval, Call, CallError, CallResult};
use crate::client::{filetime_now, Client};
use crate::services::charsel::FLAG_HANGAR;
use crate::space::{now_tick, Ball, MODE_STOP, V3};
use marshal::Value;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Mutex, OnceLock};
use tracing::info;

fn db_err(e: tokio_postgres::Error) -> CallError {
    CallError::Internal(e.into())
}

// appConst.agentMissionState*
const STATE_OFFERED: i32 = 1;
const STATE_ACCEPTED: i32 = 2;
// appConst.agentDialogueButton*
const BTN_VIEW: i64 = 1;
const BTN_REQUEST: i64 = 2;
const BTN_ACCEPT: i64 = 3;
const BTN_COMPLETE: i64 = 6;
const BTN_DECLINE: i64 = 9;
const BTN_QUIT: i64 = 11;
// Our action IDs (DoAction's argument).
const ACT_REQUEST: i64 = 1;
const ACT_VIEW: i64 = 2;
const ACT_ACCEPT: i64 = 3;
const ACT_DECLINE: i64 = 4;
const ACT_COMPLETE: i64 = 5;
const ACT_QUIT: i64 = 6;
/// Hand in a talk-to-agent or courier mission another agent gave us.
const ACT_DELIVER: i64 = 7;
/// Agent-interaction answers: ACT_OPTION + option index.
const ACT_OPTION: i64 = 10;
const AGENT_TYPE_BASIC: i64 = 2;
const TYPE_CREDITS: i64 = 29;
const TYPE_SOLAR_SYSTEM: i64 = 5;
const GROUP_PLANET: i32 = 7;
/// Owner of mission NPCs from dungeons without a faction (Guristas Pirates).
const GENERIC_NPC_CORP: i64 = 1000127;
/// Mission sites sit this far off a planet (m).
const SITE_OFFSET: f64 = 15_000_000.0;
/// A ship this close to a site's centre (m) is in it; NPCs spawn up to 32 km out.
const SITE_RADIUS: f64 = 100_000.0;
/// Temporary item IDs for spawned NPCs.
pub(super) static NEXT_NPC_ID: AtomicI64 = AtomicI64::new(9_000_000_000_000);

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

fn rand01() -> f64 {
    (crate::handshake::rand_i64() as u64 >> 11) as f64 / (1u64 << 53) as f64
}

/// A random unit vector.
fn rand_dir() -> V3 {
    let (z, a) = (rand01() * 2.0 - 1.0, rand01() * std::f64::consts::TAU);
    let r = (1.0 - z * z).sqrt();
    [r * a.cos(), z, r * a.sin()]
}

// ---------------------------------------------------------------- agents

pub struct Agent {
    pub id: i64,
    pub type_id: i64,
    pub level: i64,
    pub station_id: i64,
    pub system_id: i64,
    pub corp_id: i64,
    pub faction_id: i64,
}

/// The retail agent list: Rowset lines `[agentID, agentTypeID, divisionID, level, stationID,
/// bloodlineID, corporationID, gender, isLocatorAgent, factionID, solarSystemID]`.
fn agents() -> &'static HashMap<i64, Agent> {
    static AGENTS: OnceLock<HashMap<i64, Agent>> = OnceLock::new();
    AGENTS.get_or_init(|| {
        let mut out = HashMap::new();
        let Some(Value::Instance(_, state)) = crate::retail::cached_call("agentMgr", "GetAgents", &[]) else { return out };
        for line in state.get("lines").and_then(|l| l.items()).unwrap_or_default() {
            let Some(f) = line.items() else { continue };
            let n = |i: usize| f.get(i).and_then(|v| v.as_int()).unwrap_or(0);
            out.insert(n(0), Agent { id: n(0), type_id: n(1), level: n(3), station_id: n(4), system_id: n(10), corp_id: n(6), faction_id: n(9) });
        }
        out
    })
}

pub fn agent(id: i64) -> Option<&'static Agent> {
    agents().get(&id)
}

// ---------------------------------------------------------------- missions

/// What a mission asks for (its content template).
#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Kill,
    Talk,
    Courier,
    Trade,
    Interaction,
}

impl Kind {
    fn of_template(template: &str) -> Kind {
        match template.rsplit('_').next().unwrap_or("") {
            t if t.ends_with("TalkToAgentMission") => Kind::Talk,
            t if t.ends_with("CourierMission") => Kind::Courier,
            t if t.ends_with("TradeMission") => Kind::Trade,
            t if t.ends_with("AgentInteractionMission") => Kind::Interaction,
            _ => Kind::Kill,
        }
    }

    fn code(self) -> &'static str {
        match self {
            Kind::Kill => "kill",
            Kind::Talk => "talk",
            Kind::Courier => "courier",
            Kind::Trade => "trade",
            Kind::Interaction => "interaction",
        }
    }

    fn of_code(code: &str) -> Kind {
        match code {
            "talk" => Kind::Talk,
            "courier" => Kind::Courier,
            "trade" => Kind::Trade,
            "interaction" => Kind::Interaction,
            _ => Kind::Kill,
        }
    }

    /// The journal's mission type label (UI/Agents/MissionTypes/*).
    fn journal_label(self, arc: bool) -> &'static str {
        match (self, arc) {
            (Kind::Kill, false) => "UI/Agents/MissionTypes/Encounter",
            (Kind::Kill, true) => "UI/Agents/MissionTypes/EpicArcEncounter",
            (Kind::Talk, _) => "UI/Agents/MissionTypes/EpicArcTalkToAgent",
            (Kind::Courier, true) => "UI/Agents/MissionTypes/EpicArcCourier",
            (Kind::Courier, false) => "UI/Agents/MissionTypes/Courier",
            (Kind::Trade, true) => "UI/Agents/MissionTypes/EpicArcTrade",
            (Kind::Trade, false) => "UI/Agents/MissionTypes/Trade",
            (Kind::Interaction, _) => "UI/Agents/MissionTypes/EpicArcAgentInteraction",
        }
    }
}

struct Mission {
    agent_id: i64,
    mission_id: i32,
    name_id: i64,
    briefing_id: i64,
    dungeon_id: i32,
    faction_id: i64,
    state: i32,
    system_id: i64,
    pos: V3,
    reward_isk: i64,
    reward_lp: i64,
    accepted_at: Option<i64>,
    kind: Kind,
    /// The epic arc this mission is a step of.
    arc_id: Option<i64>,
    /// Where a talk-to-agent or courier mission is handed in (None: the giver).
    dest_agent: Option<i64>,
    /// (typeID, quantity) a courier mission carries or a trade mission wants.
    cargo: Option<(i32, i32)>,
}

impl Mission {
    fn hand_in_agent(&self) -> i64 {
        self.dest_agent.unwrap_or(self.agent_id)
    }
}

const MISSION_COLUMNS: &str = "am.agent_id, am.mission_id, (m.data->>'nameID')::bigint, (m.data->'messages'->>'messages.mission.briefing')::bigint,
    am.dungeon_id, am.faction_id, am.state, am.system_id, am.x, am.y, am.z, am.reward_isk, am.reward_lp, am.accepted_at,
    am.kind, am.arc_id, am.dest_agent, am.cargo_type, am.cargo_qty";

fn mission_from(r: &tokio_postgres::Row) -> Mission {
    Mission {
        agent_id: r.get(0),
        mission_id: r.get(1),
        name_id: r.get::<_, Option<i64>>(2).unwrap_or(0),
        briefing_id: r.get::<_, Option<i64>>(3).unwrap_or(0),
        dungeon_id: r.get(4),
        faction_id: r.get(5),
        state: r.get(6),
        system_id: r.get(7),
        pos: [r.get(8), r.get(9), r.get(10)],
        reward_isk: r.get(11),
        reward_lp: r.get(12),
        accepted_at: r.get(13),
        kind: Kind::of_code(r.get(14)),
        arc_id: r.get(15),
        dest_agent: r.get(16),
        cargo: r.get::<_, Option<i32>>(17).zip(r.get::<_, Option<i32>>(18)),
    }
}

async fn missions_of(c: &Client, agent_id: Option<i64>) -> Result<Vec<Mission>, CallError> {
    let me = c.session.int("charid").unwrap_or(0);
    let rows = c
        .db
        .client
        .query(
            &format!(
                "SELECT {MISSION_COLUMNS} FROM game.agent_missions am JOIN sde_native.missions m ON m.key = am.mission_id::text
                 WHERE am.character_id = $1 AND ($2::bigint IS NULL OR am.agent_id = $2) ORDER BY am.offered_at"
            ),
            &[&me, &agent_id],
        )
        .await
        .map_err(db_err)?;
    Ok(rows.iter().map(mission_from).collect())
}

async fn mission_with(c: &Client, agent_id: i64) -> Result<Option<Mission>, CallError> {
    Ok(missions_of(c, Some(agent_id)).await?.into_iter().next())
}

/// A mission message ID of this mission (`messages.mission.accepted.agentsays`...).
async fn message(c: &Client, mission_id: i32, key: &str) -> Result<Option<i64>, CallError> {
    Ok(c.db
        .client
        .query_one("SELECT (data->'messages'->>$2)::bigint FROM sde_native.missions WHERE key = $1::int::text", &[&mission_id, &key])
        .await
        .map_err(db_err)?
        .get(0))
}

/// A mission site: a point well off a random planet of the system.
async fn site_position(c: &Client, system: i64) -> Result<V3, CallError> {
    let planet = c
        .db
        .client
        .query_opt(
            "SELECT x, y, z FROM sde.map_celestials WHERE solar_system_id = $1 AND group_id = $2 ORDER BY random() LIMIT 1",
            &[&(system as i32), &GROUP_PLANET],
        )
        .await
        .map_err(db_err)?;
    let base: V3 = planet.map(|p| [p.get(0), p.get(1), p.get(2)]).unwrap_or([0.0; 3]);
    let d = rand_dir();
    Ok([base[0] + d[0] * SITE_OFFSET, base[1] + d[1] * SITE_OFFSET * 0.1, base[2] + d[2] * SITE_OFFSET])
}

/// Offer one of the build's kill missions at the agent's level, sited in the agent's system.
async fn offer(c: &Client, agent: &Agent) -> Result<(), CallError> {
    let me = c.session.int("charid").unwrap_or(0);
    let level = agent.level.clamp(1, 5) as i32;
    let pick = c
        .db
        .client
        .query_opt(
            "SELECT m.key::int, (m.data->'killMission'->>'dungeonID')::int, COALESCE((d.data->>'factionID')::bigint, 0)
             FROM sde_native.missions m JOIN sde_native.dungeons d ON d.key = m.data->'killMission'->>'dungeonID'
             WHERE m.data->>'contentTemplate' = 'agent.missionTemplatizedContent_BasicKillMission'
               AND (d.data->>'difficulty')::int = $1 AND m.data ? 'nameID' AND m.data->'messages' ? 'messages.mission.briefing'
             ORDER BY random() LIMIT 1",
            &[&level],
        )
        .await
        .map_err(db_err)?
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("no level {level} kill missions")))?;
    let pos = site_position(c, agent.system_id).await?;
    let (isk, lp) = rewards(agent.level);
    c.db.client
        .execute(
            "INSERT INTO game.agent_missions (character_id, agent_id, mission_id, dungeon_id, faction_id, state, system_id, x, y, z, reward_isk, reward_lp, offered_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
             ON CONFLICT (character_id, agent_id) DO NOTHING",
            &[
                &me,
                &agent.id,
                &pick.get::<_, i32>(0),
                &pick.get::<_, i32>(1),
                &pick.get::<_, i64>(2),
                &STATE_OFFERED,
                &agent.system_id,
                &pos[0],
                &pos[1],
                &pos[2],
                &isk,
                &lp,
                &filetime_now(),
            ],
        )
        .await
        .map_err(db_err)?;
    info!(user = c.login.user_id, "agent {}: offered mission {} (dungeon {})", agent.id, pick.get::<_, i32>(0), pick.get::<_, i32>(1));
    Ok(())
}

// ---------------------------------------------------------------- epic arcs

/// One step of an epic arc.
struct ArcStep {
    arc: i64,
    agent: i64,
    next: Vec<i32>,
    fail: Option<i32>,
    /// The arc begins here (no step leads to it).
    start: bool,
}

/// Every arc step by mission ID (sde_native.epicarcs), loaded once.
async fn arcs(c: &Client) -> Result<&'static HashMap<i32, ArcStep>, CallError> {
    static ARCS: OnceLock<HashMap<i32, ArcStep>> = OnceLock::new();
    if let Some(a) = ARCS.get() {
        return Ok(a);
    }
    let rows = c
        .db
        .client
        .query(
            "SELECT e.key::bigint, m.key::int, (m.value->>'agentID')::bigint, COALESCE(m.value->'nextMissions', '[]'::jsonb)::text,
                    (m.value->>'failMissionID')::int
             FROM sde_native.epicarcs e, jsonb_each(e.data->'missions') m",
            &[],
        )
        .await
        .map_err(db_err)?;
    let mut steps: HashMap<i32, ArcStep> = HashMap::new();
    for r in &rows {
        let next: Vec<i32> = r.get::<_, String>(3).trim_matches(|c| c == '[' || c == ']').split(',').filter_map(|n| n.trim().parse().ok()).collect();
        steps.insert(r.get(1), ArcStep { arc: r.get(0), agent: r.get::<_, Option<i64>>(2).unwrap_or(0), next, fail: r.get(4), start: true });
    }
    let followers: HashSet<i32> = steps.values().flat_map(|s| s.next.iter().copied()).collect();
    for (id, s) in steps.iter_mut() {
        s.start = !followers.contains(id);
    }
    Ok(ARCS.get_or_init(|| steps))
}

async fn is_arc_agent(c: &Client, agent_id: i64) -> Result<bool, CallError> {
    Ok(arcs(c).await?.values().any(|s| s.agent == agent_id))
}

/// The arc step this agent has for us next: the start of an arc we haven't begun, or a
/// step our progress has unlocked.
async fn next_arc_step(c: &Client, agent_id: i64) -> Result<Option<i32>, CallError> {
    let me = c.session.int("charid").unwrap_or(0);
    let rows = c
        .db
        .client
        .query("SELECT arc_id, mission_id, state FROM game.epic_arc_missions WHERE character_id = $1", &[&me])
        .await
        .map_err(db_err)?;
    let begun: HashSet<i64> = rows.iter().map(|r| r.get(0)).collect();
    let unlocked: HashSet<i32> = rows.iter().filter(|r| r.get::<_, String>(2) == "unlocked").map(|r| r.get(1)).collect();
    let mut mine: Vec<(&i32, &ArcStep)> = arcs(c).await?.iter().filter(|(_, s)| s.agent == agent_id).collect();
    mine.sort_by_key(|(id, _)| **id);
    Ok(mine.into_iter().find(|(id, s)| unlocked.contains(id) || (s.start && !begun.contains(&s.arc))).map(|(id, _)| *id))
}

async fn set_arc_state(c: &Client, arc: i64, mission: i32, state: &str) -> Result<(), CallError> {
    let me = c.session.int("charid").unwrap_or(0);
    let column = match state {
        "accepted" => "accepted_at",
        "completed" => "completed_at",
        "quit" => "quit_at",
        _ => "accepted_at",
    };
    let stamp = if state == "unlocked" { None } else { Some(filetime_now()) };
    c.db.client
        .execute(
            &format!(
                "INSERT INTO game.epic_arc_missions (character_id, arc_id, mission_id, state, {column}) VALUES ($1, $2, $3, $4, $5)
                 ON CONFLICT (character_id, arc_id, mission_id) DO UPDATE SET state = EXCLUDED.state, {column} = COALESCE(EXCLUDED.{column}, game.epic_arc_missions.{column})"
            ),
            &[&me, &arc, &mission, &state, &stamp],
        )
        .await
        .map_err(db_err)?;
    Ok(())
}

/// Offer an epic arc step: its content (kill site, courier cargo, trade goods...) and
/// rewards come from the mission's own data.
async fn offer_arc(c: &Client, agent: &Agent, mission_id: i32) -> Result<(), CallError> {
    let me = c.session.int("charid").unwrap_or(0);
    let steps = arcs(c).await?;
    let Some(step) = steps.get(&mission_id) else { return Ok(()) };
    let r = c
        .db
        .client
        .query_one(
            "SELECT m.data->>'contentTemplate', COALESCE((m.data->'killMission'->>'dungeonID')::int, 0),
                    COALESCE((d.data->>'factionID')::bigint, (m.data->>'factionID')::bigint, 0),
                    (m.data->'courierMission'->>'objectiveTypeID')::int, (m.data->'courierMission'->>'objectiveQuantity')::int,
                    CASE WHEN (m.data->'missionRewards'->'reward'->>'rewardTypeID')::int = $2
                         THEN (m.data->'missionRewards'->'reward'->>'rewardQuantity')::bigint ELSE 0 END,
                    COALESCE((m.data->>'fixedLpRewardOmega')::bigint, 0)
             FROM sde_native.missions m LEFT JOIN sde_native.dungeons d ON d.key = m.data->'killMission'->>'dungeonID'
             WHERE m.key = $1::text",
            &[&mission_id.to_string(), &(TYPE_CREDITS as i32)],
        )
        .await
        .map_err(db_err)?;
    let kind = Kind::of_template(&r.get::<_, Option<String>>(0).unwrap_or_default());
    // Talk-to-agent and courier steps end at the agent of the step that follows.
    let dest = match kind {
        Kind::Talk | Kind::Courier => step.next.first().and_then(|n| steps.get(n)).map(|s| s.agent).filter(|a| *a != agent.id),
        _ => None,
    };
    let cargo_type: Option<i32> = r.get(3);
    let cargo_qty: Option<i32> = cargo_type.map(|_| r.get::<_, Option<i32>>(4).unwrap_or(1).max(1));
    let pos = site_position(c, agent.system_id).await?;
    c.db.client
        .execute(
            "INSERT INTO game.agent_missions (character_id, agent_id, mission_id, dungeon_id, faction_id, state, system_id, x, y, z,
                 reward_isk, reward_lp, offered_at, kind, arc_id, dest_agent, cargo_type, cargo_qty)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18)
             ON CONFLICT (character_id, agent_id) DO NOTHING",
            &[
                &me,
                &agent.id,
                &mission_id,
                &r.get::<_, i32>(1),
                &r.get::<_, i64>(2),
                &STATE_OFFERED,
                &agent.system_id,
                &pos[0],
                &pos[1],
                &pos[2],
                &r.get::<_, i64>(5),
                &r.get::<_, i64>(6),
                &filetime_now(),
                &kind.code(),
                &step.arc,
                &dest,
                &cargo_type,
                &cargo_qty,
            ],
        )
        .await
        .map_err(db_err)?;
    info!(user = c.login.user_id, "agent {}: offered epic arc {} step {mission_id} ({kind:?})", agent.id, step.arc);
    Ok(())
}

/// agentMgr.GetMyEpicArcStatus() -> {arcID: {missionID: (acceptedDate, completedDate,
/// quitDate, nameID)}} (epicArcController._GetMissionStatus).
async fn epic_arc_status(c: &Client) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let rows = c
        .db
        .client
        .query(
            "SELECT e.arc_id, e.mission_id, e.accepted_at, e.completed_at, e.quit_at, (m.data->>'nameID')::bigint
             FROM game.epic_arc_missions e JOIN sde_native.missions m ON m.key = e.mission_id::text
             WHERE e.character_id = $1 AND e.state <> 'unlocked' ORDER BY e.arc_id",
            &[&me],
        )
        .await
        .map_err(db_err)?;
    let mut by_arc: Vec<(i64, Vec<(Value, Value)>)> = vec![];
    for r in rows {
        let arc: i64 = r.get(0);
        let stamp = |i: usize| r.get::<_, Option<i64>>(i).map(Value::Int).unwrap_or(Value::None);
        let status = keyval([("acceptedDate", stamp(2)), ("completedDate", stamp(3)), ("quitDate", stamp(4)), ("nameID", stamp(5))]);
        match by_arc.last_mut() {
            Some((a, list)) if *a == arc => list.push((Value::Int(r.get::<_, i32>(1) as i64), status)),
            _ => by_arc.push((arc, vec![(Value::Int(r.get::<_, i32>(1) as i64), status)])),
        }
    }
    Ok(Value::Dict(by_arc.into_iter().map(|(arc, list)| (Value::Int(arc), Value::Dict(list))).collect()))
}

// ---------------------------------------------------------------- cargo

/// Where a pilot can hand goods in at `station`: its hangar there, or its ship's cargo
/// while docked there.
async fn goods_at(c: &Client, station: i64, type_id: i32) -> Result<Vec<super::dogma::Item>, CallError> {
    let me = c.session.int("charid").unwrap_or(0);
    let ship = if c.session.int("stationid") == Some(station) { c.session.int("shipid").unwrap_or(0) } else { 0 };
    super::dogma::load_items(
        c,
        "i.owner_id = $1 AND i.type_id = $2 AND ((i.location_id = $3 AND i.flag_id = $4) OR (i.location_id = $5 AND i.flag_id = $6))",
        &[&me, &type_id, &station, &FLAG_HANGAR, &ship, &super::combat::FLAG_CARGO],
    )
    .await
}

async fn has_goods(c: &Client, station: i64, (type_id, qty): (i32, i32)) -> Result<bool, CallError> {
    Ok(goods_at(c, station, type_id).await?.iter().map(|i| i.quantity as i64).sum::<i64>() >= qty as i64)
}

/// Take `qty` of the goods from the hangar and cargo at `station`.
async fn take_goods(c: &mut Client, station: i64, (type_id, qty): (i32, i32)) -> Result<(), CallError> {
    let mut left = qty;
    for item in goods_at(c, station, type_id).await? {
        if left <= 0 {
            break;
        }
        if item.quantity <= left {
            left -= item.quantity;
            super::combat::remove_stack(c, item).await?;
        } else {
            c.db.client.execute("UPDATE game.items SET quantity = quantity - $2 WHERE item_id = $1", &[&item.item_id, &left]).await.map_err(db_err)?;
            super::inventory::notify_item_change(c, item.item_id, vec![(super::combat::IX_STACK_SIZE, Value::Int(item.quantity as i64))]).await?;
            left = 0;
        }
    }
    Ok(())
}

/// Can mission `m` be handed in to `agent` now?
async fn ready(c: &Client, m: &Mission, agent: &Agent) -> Result<bool, CallError> {
    if m.state != STATE_ACCEPTED || m.hand_in_agent() != agent.id {
        return Ok(false);
    }
    Ok(match m.kind {
        Kind::Kill => objective_done(c, m),
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
async fn finish(c: &mut Client, m: &Mission, agent: &Agent, choice: Option<usize>) -> Result<(), CallError> {
    let me = c.session.int("charid").unwrap_or(0);
    if matches!(m.kind, Kind::Courier | Kind::Trade) {
        if let Some(cargo) = m.cargo {
            take_goods(c, agent.station_id, cargo).await?;
        }
    }
    remove(c, m.agent_id).await?;
    if m.reward_isk > 0 {
        let balance: f64 = c
            .db
            .client
            .query_one("UPDATE game.characters SET balance = balance + $2::bigint WHERE character_id = $1 RETURNING balance::float8", &[&me, &m.reward_isk])
            .await
            .map_err(db_err)?
            .get(0);
        c.notify("OnAccountChange", vec![Value::str("cash"), Value::Int(me), Value::Float(balance)]);
    }
    // Loyalty points with the agent's corporation, and standings with agent, corporation and faction.
    super::standings::add_lp(&c.db, &c.world, me, agent.corp_id, m.reward_lp).await?;
    super::standings::mission_completed(&c.db, &c.world, me, agent.id, agent.corp_id, agent.faction_id, agent.level).await?;
    if let Some(arc) = m.arc_id {
        let item = c
            .db
            .client
            .query_opt(
                "SELECT (data->'missionRewards'->'reward'->>'rewardTypeID')::int, (data->'missionRewards'->'reward'->>'rewardQuantity')::int
                 FROM sde_native.missions WHERE key = $1::text AND (data->'missionRewards'->'reward'->>'rewardTypeID')::int <> $2",
                &[&m.mission_id.to_string(), &(TYPE_CREDITS as i32)],
            )
            .await
            .map_err(db_err)?;
        if let Some(r) = item {
            let (type_id, qty): (Option<i32>, Option<i32>) = (r.get(0), r.get(1));
            if let (Some(type_id), Some(qty)) = (type_id, qty) {
                super::combat::deposit(c, me, agent.station_id, FLAG_HANGAR, type_id, qty.max(1), 0).await?;
            }
        }
        set_arc_state(c, arc, m.mission_id, "completed").await?;
        let next = arcs(c).await?.get(&m.mission_id).map(|s| s.next.clone()).unwrap_or_default();
        let unlocked: Vec<i32> = match choice {
            Some(k) => next.get(k).or(next.last()).copied().into_iter().collect(),
            None => next,
        };
        for n in unlocked {
            set_arc_state(c, arc, n, "unlocked").await?;
        }
    }
    c.notify("OnAgentMissionChange", vec![Value::str("completed"), Value::Int(m.agent_id)]);
    info!(user = c.login.user_id, "agent {}: mission {} completed at {}, {} ISK", m.agent_id, m.mission_id, agent.id, m.reward_isk);
    Ok(())
}

/// An agent interaction's answers: messages.mission.option<N>.charsays.
async fn options(c: &Client, mission_id: i32) -> Result<Vec<i64>, CallError> {
    let mut out = vec![];
    for n in 1..=4 {
        match message(c, mission_id, &format!("messages.mission.option{n}.charsays")).await? {
            Some(id) => out.push(id),
            None => break,
        }
    }
    Ok(out)
}

async fn set_state(c: &Client, agent_id: i64, state: i32) -> Result<(), CallError> {
    let me = c.session.int("charid").unwrap_or(0);
    c.db.client
        .execute(
            "UPDATE game.agent_missions SET state = $3, accepted_at = CASE WHEN $3 = 2 THEN $4 ELSE accepted_at END
             WHERE character_id = $1 AND agent_id = $2",
            &[&me, &agent_id, &state, &filetime_now()],
        )
        .await
        .map_err(db_err)?;
    Ok(())
}

async fn remove(c: &Client, agent_id: i64) -> Result<(), CallError> {
    let me = c.session.int("charid").unwrap_or(0);
    c.db.client.execute("DELETE FROM game.agent_missions WHERE character_id = $1 AND agent_id = $2", &[&me, &agent_id]).await.map_err(db_err)?;
    despawn(c, me, agent_id);
    Ok(())
}

// ---------------------------------------------------------------- sites and NPCs

/// (character, agent) -> (solar system, NPCs spawned at the site).
fn sites() -> &'static Mutex<HashMap<(i64, i64), (i64, Vec<i64>)>> {
    static SITES: OnceLock<Mutex<HashMap<(i64, i64), (i64, Vec<i64>)>>> = OnceLock::new();
    SITES.get_or_init(Default::default)
}

/// NPC hull classes per agent level.
fn spawn_classes(level: i64) -> Vec<&'static str> {
    let (classes, n): (&[&str], usize) = match level {
        1 => (&["Frigate", "Frigate", "Destroyer"], 4),
        2 => (&["Frigate", "Destroyer", "Cruiser"], 5),
        3 => (&["Cruiser", "Cruiser", "Battlecruiser"], 6),
        4 => (&["Battlecruiser", "Battleship", "Battleship"], 7),
        _ => (&["Battleship"], 8),
    };
    (0..n).map(|i| classes[i % classes.len()]).collect()
}

/// Does an NPC group name (`Mission Caldari State Cruiser`, `Deadspace Guristas Battleship`,
/// `Mission Generic Cruisers`) carry ships of `class`?
fn group_has_class(group: &str, class: &str) -> bool {
    let g = group.to_ascii_lowercase().replace("battle cruiser", "battlecruiser");
    let class = class.to_ascii_lowercase();
    match class.as_str() {
        "cruiser" => g.contains("cruiser") && !g.contains("battlecruiser"),
        c => g.contains(c),
    }
}

/// Spawn the mission's NPCs around its site, unless already there.
async fn ensure_site(c: &mut Client, m: &Mission) -> Result<(), CallError> {
    let me = c.session.int("charid").unwrap_or(0);
    if sites().lock().unwrap().contains_key(&(me, m.agent_id)) {
        return Ok(());
    }
    // The dungeon faction's mission/deadspace rats, else the generic mission rats.
    let rows = c
        .db
        .client
        .query(
            "SELECT t.type_id, g.name, COALESCE(t.radius, 50)::float8, COALESCE(t.mass, 1000000)::float8,
                    (SELECT value FROM sde.type_attributes WHERE type_id = t.type_id AND attribute_id = 37),
                    (SELECT value FROM sde.type_attributes WHERE type_id = t.type_id AND attribute_id = 70),
                    g.name ILIKE 'Mission Generic%'
             FROM sde.inv_types t JOIN sde.inv_groups g ON g.group_id = t.group_id
             LEFT JOIN sde.chr_factions f ON f.faction_id::bigint = $1
             WHERE g.category_id = 11
               AND (g.name ILIKE 'Mission ' || COALESCE(f.name, '~') || ' %'
                    OR g.name ILIKE 'Deadspace ' || split_part(COALESCE(f.name, '~'), ' ', 1) || '%'
                    OR g.name ILIKE 'Mission Generic%')",
            &[&m.faction_id],
        )
        .await
        .map_err(db_err)?;
    // NPCs belong to the faction's NPC corporation: the client looks owners and corpIDs up
    // as corporations (tickers, standings), and a faction ID there breaks target locking.
    let corp: i64 = c
        .db
        .client
        .query_opt("SELECT corporation_id::bigint FROM sde.chr_factions WHERE faction_id::bigint = $1", &[&m.faction_id])
        .await
        .map_err(db_err)?
        .and_then(|r| r.get::<_, Option<i64>>(0))
        .unwrap_or(GENERIC_NPC_CORP);
    let level = agent(m.agent_id).map(|a| a.level).unwrap_or(1);
    let tick = now_tick();
    let mut spawned = Vec::new();
    for class in spawn_classes(level) {
        let fits: Vec<&tokio_postgres::Row> = rows.iter().filter(|r| group_has_class(&r.get::<_, String>(1), class)).collect();
        let faction: Vec<&&tokio_postgres::Row> = fits.iter().filter(|r| !r.get::<_, bool>(6)).collect();
        let pool: Vec<&tokio_postgres::Row> = if faction.is_empty() { fits.clone() } else { faction.into_iter().copied().collect() };
        if pool.is_empty() {
            continue;
        }
        let r = pool[(rand01() * pool.len() as f64) as usize % pool.len()];
        let d = rand_dir();
        let dist = 12_000.0 + rand01() * 20_000.0;
        let ball = Ball {
            id: NEXT_NPC_ID.fetch_add(1, Ordering::Relaxed),
            type_id: r.get(0),
            owner_id: corp,
            corp_id: corp,
            alliance_id: -1,
            char_id: None,
            mode: MODE_STOP,
            radius: r.get::<_, f64>(2) as f32,
            mass: r.get(3),
            max_vel: r.get::<_, Option<f64>>(4).unwrap_or(300.0) as f32,
            agility: r.get::<_, Option<f64>>(5).unwrap_or(3.0) as f32,
            speed_fraction: 0.0,
            pos: [m.pos[0] + d[0] * dist, m.pos[1] + d[1] * dist * 0.3, m.pos[2] + d[2] * dist],
            vel: [0.0; 3],
            goto: [0.0; 3],
            follow_id: 0,
            follow_range: 0.0,
            free: true,
            global: false,
            massive: true,
            warp: None,
            warp_factor: 0,
            last_tick: tick,
            cloak: 0,
            cloak_until: 0,
            health: [1.0; 3],
            launcher: 0,
            slim: vec![("corpID", Value::Int(corp))],
        };
        spawned.push(ball.id);
        let (id, type_id) = (ball.id, ball.type_id);
        c.world.add_ship(m.system_id, ball).await.map_err(CallError::Internal)?;
        super::combat::spawn_npc(c.db.clone(), c.world.clone(), m.system_id, id, type_id, Some(me));
    }
    info!(user = c.login.user_id, "agent {}: spawned {} NPCs for mission {} in {}", m.agent_id, spawned.len(), m.mission_id, m.system_id);
    sites().lock().unwrap().insert((me, m.agent_id), (m.system_id, spawned));
    Ok(())
}

fn despawn(c: &Client, char_id: i64, agent_id: i64) {
    if let Some((system, npcs)) = sites().lock().unwrap().remove(&(char_id, agent_id)) {
        for id in npcs {
            c.world.remove_ball(system, id);
        }
    }
}

/// Is the mission's objective done: its site spawned and every NPC there gone?
fn objective_done(c: &Client, m: &Mission) -> bool {
    let me = c.session.int("charid").unwrap_or(0);
    let Some((system, npcs)) = sites().lock().unwrap().get(&(me, m.agent_id)).cloned() else { return false };
    let balls = c.world.balls_snapshot(system).unwrap_or_default();
    m.state == STATE_ACCEPTED && !npcs.iter().any(|id| balls.contains_key(id))
}

// ---------------------------------------------------------------- client shapes

fn keywords(m: &Mission) -> Value {
    Value::dict([("dungeonLocationID", Value::Int(m.system_id)), ("dungeonSolarSystemID", Value::Int(m.system_id))])
}

/// A location for agentDialogueUtil.LocationWrapper.
fn site_location(m: &Mission) -> Value {
    Value::dict([
        ("locationID", Value::Int(m.system_id)),
        ("locationType", Value::str("dungeon")),
        ("solarsystemID", Value::Int(m.system_id)),
        ("typeID", Value::Int(TYPE_SOLAR_SYSTEM)),
        ("coords", Value::tuple(m.pos.map(Value::Float))),
        ("agentID", Value::Int(m.agent_id)),
    ])
}

/// The agent's station: (stationID, typeID, solarSystemID, position).
async fn agent_station(c: &Client, agent: &Agent) -> Result<(i64, i64, i64, V3), CallError> {
    let r = c
        .db
        .client
        .query_opt(
            "SELECT s.type_id::bigint, s.solar_system_id::bigint, COALESCE(s.x, 0), COALESCE(s.y, 0), COALESCE(s.z, 0)
             FROM sde.sta_stations s WHERE s.station_id = $1::int",
            &[&(agent.station_id as i32)],
        )
        .await
        .map_err(db_err)?;
    Ok(match r {
        Some(r) => (agent.station_id, r.get(0), r.get(1), [r.get(2), r.get(3), r.get(4)]),
        None => (agent.station_id, 0, agent.system_id, [0.0; 3]),
    })
}

/// Where an agent sits, as a LocationWrapper dict.
async fn agent_location(c: &Client, agent_id: i64) -> Result<Value, CallError> {
    let Some(a) = agent(agent_id) else { return Ok(Value::None) };
    let (station, ty, system, _) = agent_station(c, a).await?;
    Ok(Value::dict([
        ("locationID", Value::Int(station)),
        ("locationType", Value::str("station")),
        ("solarsystemID", Value::Int(system)),
        ("typeID", Value::Int(ty)),
    ]))
}

/// GetMissionObjectiveInfo: kill missions list their site, the others their hand-in
/// (agentDialogueUtil._ProcessObjectiveEntry: 'agent', 'transport', 'fetch').
async fn objective_info(c: &Client, m: &Mission) -> Result<Value, CallError> {
    if m.kind == Kind::Kill {
        return Ok(kill_objective_info(c, m));
    }
    let hand_in = m.hand_in_agent();
    let mut objectives = vec![];
    let cargo_value = |has: bool, (type_id, qty): (i32, i32), volume: f64| {
        Value::dict([("hasCargo", Value::Bool(has)), ("quantity", Value::Int(qty as i64)), ("typeID", Value::Int(type_id as i64)), ("volume", Value::Float(volume))])
    };
    let volume = |c: &Client, type_id: i32| {
        let db = c.db.clone();
        async move {
            db.client
                .query_opt("SELECT COALESCE(volume, 0)::float8 FROM sde.inv_types WHERE type_id = $1", &[&type_id])
                .await
                .ok()
                .flatten()
                .map(|r| r.get::<_, f64>(0))
                .unwrap_or(0.0)
        }
    };
    match (m.kind, m.cargo) {
        (Kind::Talk, _) => objectives.push(Value::tuple([Value::str("agent"), Value::tuple([Value::Int(hand_in), agent_location(c, hand_in).await?])])),
        (Kind::Courier, Some(cargo)) => {
            let station = agent(hand_in).map(|a| a.station_id).unwrap_or(0);
            let has = has_goods(c, station, cargo).await? || m.state == STATE_ACCEPTED;
            let data = Value::tuple([
                Value::Int(m.agent_id),
                agent_location(c, m.agent_id).await?,
                Value::Int(hand_in),
                agent_location(c, hand_in).await?,
                cargo_value(has, cargo, volume(c, cargo.0).await * cargo.1 as f64),
            ]);
            objectives.push(Value::tuple([Value::str("transport"), data]));
        }
        (Kind::Trade, Some(cargo)) => {
            let station = agent(hand_in).map(|a| a.station_id).unwrap_or(0);
            let has = has_goods(c, station, cargo).await?;
            let data = Value::tuple([Value::Int(hand_in), agent_location(c, hand_in).await?, cargo_value(has, cargo, volume(c, cargo.0).await * cargo.1 as f64)]);
            objectives.push(Value::tuple([Value::str("fetch"), data]));
        }
        _ => {}
    }
    let system = agent(hand_in).map(|a| a.system_id).unwrap_or(m.system_id);
    Ok(Value::dict([
        ("importantStandings", Value::Int(0)),
        ("completionStatus", Value::Int(0)),
        ("missionTitleID", Value::Int(m.name_id)),
        ("missionState", Value::Int(m.state as i64)),
        ("objectives", Value::List(objectives)),
        ("dungeons", Value::list([])),
        ("locations", Value::list([Value::Int(system)])),
        ("agentGift", Value::list([])),
        ("normalRewards", Value::list([Value::tuple([Value::Int(TYPE_CREDITS), Value::Int(m.reward_isk), Value::None])])),
        ("loyaltyPoints", Value::Int(m.reward_lp)),
        ("researchPoints", Value::Int(0)),
        ("bonusRewards", Value::list([])),
        ("collateral", Value::list([])),
        ("contentID", Value::Int(m.mission_id as i64)),
    ]))
}

fn kill_objective_info(c: &Client, m: &Mission) -> Value {
    let done = objective_done(c, m);
    let mut dungeon = vec![
        ("dungeonID", Value::Int(m.dungeon_id as i64)),
        ("optional", Value::Bool(false)),
        ("objectiveCompleted", if done { Value::Int(1) } else { Value::None }),
        ("location", site_location(m)),
        ("ownerID", Value::Int(m.faction_id)),
    ];
    if done {
        dungeon.push(("completionStatus", Value::Int(1)));
    }
    Value::dict([
        ("importantStandings", Value::Int(0)),
        ("completionStatus", Value::Int(i64::from(done))),
        ("missionTitleID", Value::Int(m.name_id)),
        ("missionState", Value::Int(m.state as i64)),
        ("objectives", Value::list([])),
        ("dungeons", Value::list([Value::dict(dungeon)])),
        ("locations", Value::list([Value::Int(m.system_id)])),
        ("agentGift", Value::list([])),
        ("normalRewards", Value::list([Value::tuple([Value::Int(TYPE_CREDITS), Value::Int(m.reward_isk), Value::None])])),
        ("loyaltyPoints", Value::Int(m.reward_lp)),
        ("researchPoints", Value::Int(0)),
        ("bonusRewards", Value::list([])),
        ("collateral", Value::list([])),
        ("contentID", Value::Int(m.mission_id as i64)),
    ])
}

/// appConst.agentMissionBriefing* keys.
fn briefing_info(m: &Mission) -> Value {
    Value::dict([
        ("ContentID", Value::Int(m.mission_id as i64)),
        ("Decline Time", Value::None),
        ("Expiration Time", Value::None),
        ("Mission Title ID", Value::Int(m.name_id)),
        ("Mission Briefing ID", Value::Int(m.briefing_id)),
        ("Mission Keywords", keywords(m)),
        ("Mission Image", Value::unicode("")),
        ("AcceptTimestamp", m.accepted_at.map(Value::Int).unwrap_or(Value::None)),
    ])
}

/// A journal row: (missionState, important, missionTypeLabel, missionNameID, agentID,
/// expirationTime, bookmarks, remoteOfferable, remoteCompletable, contentID).
async fn journal_row(c: &Client, m: &Mission) -> Result<Value, CallError> {
    let mut bookmarks = vec![];
    if let Some(agent) = agent(m.agent_id) {
        let (station, station_type, station_system, pos) = agent_station(c, agent).await?;
        bookmarks.push(keyval([
            ("agentID", Value::Int(m.agent_id)),
            ("locationType", Value::str("agenthomebase")),
            ("locationNumber", Value::Int(0)),
            ("hint", Value::str("UI/Agents/Bookmarks/AgentBase")),
            ("locationID", Value::Int(station_system)),
            ("itemID", Value::Int(station)),
            ("typeID", Value::Int(station_type)),
            ("x", Value::Float(pos[0])),
            ("y", Value::Float(pos[1])),
            ("z", Value::Float(pos[2])),
            ("solarsystemID", Value::Int(station_system)),
        ]));
    }
    if m.state == STATE_ACCEPTED && m.kind == Kind::Kill {
        bookmarks.push(keyval([
            ("agentID", Value::Int(m.agent_id)),
            ("locationType", Value::str("dungeon")),
            ("locationNumber", Value::Int(0)),
            ("hint", Value::str("UI/Agents/Bookmarks/Encounter/Deadspace")),
            ("locationID", Value::Int(m.system_id)),
            ("itemID", Value::Int(m.system_id)),
            ("typeID", Value::Int(TYPE_SOLAR_SYSTEM)),
            ("x", Value::Float(m.pos[0])),
            ("y", Value::Float(m.pos[1])),
            ("z", Value::Float(m.pos[2])),
            ("solarsystemID", Value::Int(m.system_id)),
        ]));
    }
    Ok(Value::tuple([
        Value::Int(m.state as i64),
        Value::Bool(false),
        Value::str(m.kind.journal_label(m.arc_id.is_some())),
        Value::Int(m.name_id),
        Value::Int(m.agent_id),
        Value::None,
        Value::List(bookmarks),
        Value::Bool(false),
        Value::Bool(false),
        Value::Int(m.mission_id as i64),
    ]))
}

async fn journal(c: &Client, agent_id: Option<i64>) -> CallResult {
    let mut rows = vec![];
    for m in missions_of(c, agent_id).await? {
        rows.push(journal_row(c, &m).await?);
    }
    // (missions, research)
    Ok(Value::tuple([Value::List(rows), Value::list([])]))
}

/// A mission message `(messageID, contentID)`, or `fallback` text when the mission has none.
async fn says(c: &Client, m: &Mission, key: &str, fallback: &str) -> Result<Value, CallError> {
    Ok(match message(c, m.mission_id, key).await? {
        Some(id) => Value::tuple([Value::Int(id), Value::Int(m.mission_id as i64)]),
        None => Value::unicode(fallback),
    })
}

fn actions(list: &[(i64, i64)]) -> Value {
    Value::list(list.iter().map(|(id, button)| Value::tuple([Value::Int(*id), Value::Int(*button)])))
}

fn last_action(completed: bool, declined: bool, quit: bool) -> Value {
    Value::dict([
        ("missionCompleted", Value::Bool(completed)),
        ("missionDeclined", Value::Bool(declined)),
        ("missionQuit", Value::Bool(quit)),
        ("missionCantReplay", Value::None),
        ("loyaltyPoints", Value::Int(0)),
    ])
}

/// An agent interaction's answers as special-interaction actions: each shows its
/// message as a link (agentDialogueWindow.GetSpecialInteractionHTML).
async fn option_actions(c: &Client, m: &Mission) -> Result<Vec<Value>, CallError> {
    Ok(options(c, m.mission_id)
        .await?
        .into_iter()
        .enumerate()
        .map(|(i, msg)| {
            let briefing = Value::dict([
                ("ContentID", Value::Int(m.mission_id as i64)),
                ("Mission Title ID", Value::Int(msg)),
                ("Mission Briefing ID", Value::None),
                ("Mission Keywords", keywords(m)),
            ]);
            Value::tuple([Value::Int(ACT_OPTION + i as i64), briefing])
        })
        .collect())
}

/// What the agent says about a mission it has with us, and what we can do about it.
async fn view(c: &Client, m: &Mission, agent: &Agent) -> Result<(Value, Value), CallError> {
    Ok(if m.state == STATE_OFFERED && m.kind == Kind::Interaction {
        let mut acts = option_actions(c, m).await?;
        acts.push(Value::tuple([Value::Int(ACT_DECLINE), Value::Int(BTN_DECLINE)]));
        (Value::tuple([Value::Int(m.briefing_id), Value::Int(m.mission_id as i64)]), Value::List(acts))
    } else if m.state == STATE_OFFERED {
        (
            Value::tuple([Value::Int(m.briefing_id), Value::Int(m.mission_id as i64)]),
            actions(&[(ACT_ACCEPT, BTN_ACCEPT), (ACT_DECLINE, BTN_DECLINE)]),
        )
    } else if ready(c, m, agent).await? {
        let done = if m.kind == Kind::Kill { "你已经清除了目标。完成任务，领取你的报酬吧。" } else { "一切就绪。完成任务，领取你的报酬吧。" };
        (Value::unicode(done), actions(&[(ACT_COMPLETE, BTN_COMPLETE), (ACT_QUIT, BTN_QUIT)]))
    } else if m.kind != Kind::Kill {
        let hint = match m.kind {
            Kind::Talk | Kind::Courier if m.dest_agent.is_some() => "去找我说的那个人吧，完成后在那里交付。",
            Kind::Courier | Kind::Trade => "把货物带到这个空间站的机库里，然后再来找我。",
            _ => "我们还有事情没办完。",
        };
        (says(c, m, "messages.root.missioninprogress.agentsays", hint).await?, actions(&[(ACT_QUIT, BTN_QUIT)]))
    } else {
        (
            says(c, m, "messages.root.missioninprogress.agentsays", "目标还在那里。完成后回来向我报告。").await?,
            actions(&[(ACT_QUIT, BTN_QUIT)]),
        )
    })
}

fn conversation(says: Value, acts: Value, last: Value) -> CallResult {
    // ((agentSays, actions), lastActionInfo)
    Ok(Value::tuple([Value::tuple([says, acts]), last]))
}

async fn do_action(c: &mut Client, agent: &Agent, action: Option<i64>) -> CallResult {
    let none = || last_action(false, false, false);
    // Talk-to-agent and courier missions another agent gave us, handed in here.
    let mut delivery = None;
    for m in missions_of(c, None).await? {
        if m.agent_id != agent.id && ready(c, &m, agent).await? {
            delivery = Some(m);
            break;
        }
    }
    if let Some(m) = delivery {
        if action == Some(ACT_DELIVER) {
            finish(c, &m, agent, None).await?;
            let s = says(c, &m, "messages.mission.completed.agentsays", "干得好。").await?;
            return conversation(s, actions(&[(ACT_REQUEST, BTN_REQUEST)]), last_action(true, false, false));
        }
        if matches!(action, None | Some(ACT_REQUEST)) {
            return conversation(Value::unicode("我一直在等你。"), actions(&[(ACT_DELIVER, BTN_COMPLETE)]), none());
        }
    }
    let arc_agent = is_arc_agent(c, agent.id).await?;
    if agent.type_id != AGENT_TYPE_BASIC && !arc_agent {
        return conversation(Value::unicode("我目前没有可以交给你的任务。"), Value::list([]), none());
    }
    let current = mission_with(c, agent.id).await?;
    match (action, current) {
        (None, None) => conversation(Value::unicode("你好，飞行员。我这里有些工作需要人手。"), actions(&[(ACT_REQUEST, BTN_REQUEST)]), none()),
        (None, Some(_)) => conversation(Value::unicode("我们还有事情没办完。"), actions(&[(ACT_VIEW, BTN_VIEW)]), none()),
        (Some(ACT_REQUEST), None) if arc_agent => {
            let Some(step) = next_arc_step(c, agent.id).await? else {
                return conversation(Value::unicode("现在我这里没有你的事。"), Value::list([]), none());
            };
            offer_arc(c, agent, step).await?;
            c.notify("OnAgentMissionChange", vec![Value::str("offered"), Value::Int(agent.id)]);
            let m = mission_with(c, agent.id).await?.ok_or_else(|| CallError::Internal(anyhow::anyhow!("offer vanished")))?;
            let (s, a) = view(c, &m, agent).await?;
            conversation(s, a, none())
        }
        (Some(ACT_REQUEST), None) => {
            let me = c.session.int("charid").unwrap_or(0);
            let needed = super::standings::required_for_level(agent.level);
            let have = super::standings::effective(&c.db, me, agent.id, agent.corp_id, agent.faction_id).await?;
            if have < needed {
                let text = format!("我只和信誉良好的飞行员合作。你需要对我、我的军团或势力拥有至少 {needed:.1} 的有效声望（目前为 {have:.2}）。");
                return conversation(Value::unicode(text), Value::list([]), none());
            }
            offer(c, agent).await?;
            c.notify("OnAgentMissionChange", vec![Value::str("offered"), Value::Int(agent.id)]);
            let m = mission_with(c, agent.id).await?.ok_or_else(|| CallError::Internal(anyhow::anyhow!("offer vanished")))?;
            let (s, a) = view(c, &m, agent).await?;
            conversation(s, a, none())
        }
        (Some(ACT_REQUEST | ACT_VIEW), Some(m)) => {
            let (s, a) = view(c, &m, agent).await?;
            conversation(s, a, none())
        }
        (Some(choice), Some(m)) if choice >= ACT_OPTION && m.state == STATE_OFFERED && m.kind == Kind::Interaction => {
            if let Some(arc) = m.arc_id {
                set_arc_state(c, arc, m.mission_id, "accepted").await?;
            }
            finish(c, &m, agent, Some((choice - ACT_OPTION) as usize)).await?;
            let s = says(c, &m, "messages.mission.completed.agentsays", "就这么定了。").await?;
            conversation(s, actions(&[(ACT_REQUEST, BTN_REQUEST)]), last_action(true, false, false))
        }
        (Some(ACT_ACCEPT), Some(m)) if m.state == STATE_OFFERED => {
            set_state(c, agent.id, STATE_ACCEPTED).await?;
            if let Some(arc) = m.arc_id {
                set_arc_state(c, arc, m.mission_id, "accepted").await?;
            }
            // Courier cargo waits in the pilot's hangar at the agent's station.
            if let (Kind::Courier, Some((type_id, qty))) = (m.kind, m.cargo) {
                let me = c.session.int("charid").unwrap_or(0);
                super::combat::deposit(c, me, agent.station_id, FLAG_HANGAR, type_id, qty, 0).await?;
            }
            info!(user = c.login.user_id, "agent {}: mission {} accepted", agent.id, m.mission_id);
            c.notify("OnAgentMissionChange", vec![Value::str("accepted"), Value::Int(agent.id)]);
            let s = says(c, &m, "messages.mission.accepted.agentsays", "很好。目标位置已经加入你的日志，出发吧。").await?;
            conversation(s, actions(&[(ACT_QUIT, BTN_QUIT)]), none())
        }
        (Some(ACT_DECLINE), Some(m)) if m.state == STATE_OFFERED => {
            remove(c, agent.id).await?;
            let me = c.session.int("charid").unwrap_or(0);
            super::standings::mission_declined(&c.db, &c.world, me, agent.id, agent.corp_id).await?;
            c.notify("OnAgentMissionChange", vec![Value::str("offer_declined"), Value::Int(agent.id)]);
            let s = says(c, &m, "messages.mission.declined.agentsays", "好吧，也许下次吧。").await?;
            conversation(s, Value::list([]), last_action(false, true, false))
        }
        (Some(ACT_COMPLETE), Some(m)) if ready(c, &m, agent).await? => {
            finish(c, &m, agent, None).await?;
            let s = says(c, &m, "messages.mission.completed.agentsays", "干得漂亮！这是你的报酬。").await?;
            let next = if m.arc_id.is_some() { actions(&[(ACT_REQUEST, BTN_REQUEST)]) } else { Value::list([]) };
            conversation(s, next, last_action(true, false, false))
        }
        (Some(ACT_QUIT), Some(m)) => {
            remove(c, agent.id).await?;
            let me = c.session.int("charid").unwrap_or(0);
            super::standings::mission_declined(&c.db, &c.world, me, agent.id, agent.corp_id).await?;
            // A quit arc step can be taken up again (its fail mission, usually itself).
            if let Some(arc) = m.arc_id {
                set_arc_state(c, arc, m.mission_id, "quit").await?;
                let retry = arcs(c).await?.get(&m.mission_id).and_then(|s| s.fail).unwrap_or(m.mission_id);
                set_arc_state(c, arc, retry, "unlocked").await?;
            }
            c.notify("OnAgentMissionChange", vec![Value::str("quit"), Value::Int(agent.id)]);
            let s = says(c, &m, "messages.mission.quitted.agentsays", "真遗憾。也许下次你能做到。").await?;
            conversation(s, Value::list([]), last_action(false, false, true))
        }
        (_, current) => match current {
            Some(m) => {
                let (s, a) = view(c, &m, agent).await?;
                conversation(s, a, none())
            }
            None => conversation(Value::unicode("你好，飞行员。"), actions(&[(ACT_REQUEST, BTN_REQUEST)]), none()),
        },
    }
}

/// WarpToLocation(locationType, locationNumber, warpRange, fleet, referringAgentID, formation)
async fn warp_to_location(c: &mut Client, agent: &Agent, call: &Call<'_>) -> CallResult {
    let kind = call.args.first().and_then(|v| v.as_text()).map(|s| s.into_owned()).unwrap_or_default();
    let range = call.args.get(2).and_then(|v| v.as_float()).unwrap_or(0.0);
    let here = c.session.int("solarsystemid");
    match kind.as_str() {
        "dungeon" => {
            let Some(m) = mission_with(c, agent.id).await?.filter(|m| m.state == STATE_ACCEPTED && m.kind == Kind::Kill) else { return Ok(Value::None) };
            if here != Some(m.system_id) {
                return Ok(Value::None);
            }
            ensure_site(c, &m).await?;
            super::ship::warp_to(c, None, m.pos, range)
        }
        "agenthomebase" => {
            let (station, _, system, pos) = agent_station(c, agent).await?;
            if here != Some(system) {
                return Ok(Value::None);
            }
            let radius = c.world.balls_snapshot(system).and_then(|b| b.get(&station).map(|s| s.radius as f64)).unwrap_or(5000.0);
            super::ship::warp_to(c, Some((pos, radius)), pos, range)
        }
        _ => Ok(Value::None),
    }
}

// ---------------------------------------------------------------- mission tracker

/// Is the pilot's ship at the mission site?
fn at_site(c: &Client, m: &Mission) -> bool {
    let (Some(system), Some(ship)) = (c.session.int("solarsystemid"), c.session.int("shipid")) else { return false };
    if system != m.system_id {
        return false;
    }
    let balls = c.world.balls_snapshot(system).unwrap_or_default();
    balls.get(&ship).is_some_and(|b| (0..3).map(|i| (b.pos[i] - m.pos[i]).powi(2)).sum::<f64>().sqrt() < SITE_RADIUS)
}

/// The steps of a kill mission, as missionObjectiveData objective items (strings).
fn tracker_objectives(m: &Mission) -> [Vec<String>; 3] {
    [vec!["TravelTo".into(), m.system_id.to_string()], vec!["DestroyAll".into()], vec!["AllObjectivesComplete".into(), m.agent_id.to_string()]]
}

/// The step the pilot is on, or None before the mission is accepted.
fn tracker_current(c: &Client, m: &Mission) -> Option<Vec<String>> {
    // Only kill missions have a site to steer to; the rest show "read the journal".
    if m.state != STATE_ACCEPTED || m.kind != Kind::Kill {
        return None;
    }
    let [travel, destroy, complete] = tracker_objectives(m);
    Some(if objective_done(c, m) {
        complete
    } else if at_site(c, m) {
        destroy
    } else {
        travel
    })
}

fn strings(items: Vec<String>) -> Value {
    Value::list(items.into_iter().map(Value::str))
}

/// missionTrackerMgr: the info panel's mission objectives (missionObjectivesTracker).
async fn tracker(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let agent_id = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
    let current = match mission_with(c, agent_id).await {
        Ok(m) => m,
        Err(e) => return Some(Err(e)),
    };
    Some(Ok(match call.method {
        "GetMissionInfoItems" => current.and_then(|m| tracker_current(c, &m)).map(strings).unwrap_or(Value::None),
        "GetAllMissionObjectives" => match current {
            Some(m) if m.state == STATE_ACCEPTED && m.kind == Kind::Kill => Value::list(tracker_objectives(&m).into_iter().map(strings)),
            _ => Value::list([]),
        },
        "IsInActiveDungeonID" => Value::Bool(current.is_some_and(|m| at_site(c, &m))),
        // UpdateAllMissions(agentIDs): answered with OnMissionsUpdated.
        "UpdateAllMissions" => {
            let agents: Vec<i64> = match call.args.first() {
                Some(Value::List(l) | Value::Tuple(l)) => l.iter().filter_map(|v| v.as_int()).collect(),
                _ => vec![],
            };
            let mut updates = vec![];
            for agent_id in agents {
                let info = match mission_with(c, agent_id).await {
                    Ok(m) => m.and_then(|m| tracker_current(c, &m)).map(strings).unwrap_or(Value::None),
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

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service == "missionTrackerMgr" {
        return tracker(c, call).await;
    }
    if call.service != "agentMgr" {
        return None;
    }
    // The bound agent: Moniker('agentMgr', agentID).
    let bound_agent = if call.bound { call.bind_params.and_then(|p| p.as_int()).and_then(agent) } else { None };
    let Some(agent) = bound_agent else {
        return match call.method {
            "GetMyJournalDetails" => Some(journal(c, None).await),
            "GetMyEpicArcStatus" => Some(epic_arc_status(c).await),
            "GetSolarSystemOfAgent" => {
                let id = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
                Some(Ok(self::agent(id).map(|a| Value::Int(a.system_id)).unwrap_or(Value::None)))
            }
            // Mission IDs switched off server-side (agents.IsMissionDisabled): none.
            "GetDisabledMissions" => Some(Ok(Value::list([]))),
            _ => None,
        };
    };
    let me = c.session.int("charid").unwrap_or(0);
    let current = match mission_with(c, agent.id).await {
        Ok(m) => m,
        Err(e) => return Some(Err(e)),
    };
    Some(match call.method {
        // The agent's show-info tab: KeyVal(stationID, level, services, incompatible).
        "GetInfoServiceDetails" => Ok(keyval([
            ("stationID", if agent.station_id > 0 { Value::Int(agent.station_id) } else { Value::None }),
            ("level", Value::Int(agent.level)),
            ("services", Value::list([])),
            ("incompatible", Value::None),
        ])),
        "DoAction" => {
            let action = call.args.first().and_then(|v| v.as_int());
            do_action(c, agent, action).await
        }
        "GetMissionBriefingInfo" => Ok(current.as_ref().map(briefing_info).unwrap_or(Value::None)),
        "GetMissionObjectiveInfo" => match &current {
            Some(m) => objective_info(c, m).await,
            None => Ok(Value::None),
        },
        "GetMissionJournalInfo" => match &current {
            Some(m) => match objective_info(c, m).await {
                Ok(objectives) => Ok(Value::dict([
                    ("missionNameID", Value::Int(m.name_id)),
                    ("contentID", Value::Int(m.mission_id as i64)),
                    ("briefingTextID", Value::Int(m.briefing_id)),
                    ("missionImage", Value::unicode("")),
                    ("expirationTime", Value::None),
                    ("missionState", Value::Int(m.state as i64)),
                    ("objectives", objectives),
                ])),
                Err(e) => Err(e),
            },
            None => Ok(Value::None),
        },
        "GetMissionKeywords" => Ok(current.as_ref().map(keywords).unwrap_or(Value::Dict(vec![]))),
        "GetAgentLocationWrap" => agent_station(c, agent).await.map(|(station, ty, system, _)| {
            Value::dict([
                ("locationID", Value::Int(station)),
                ("locationType", Value::str("station")),
                ("solarsystemID", Value::Int(system)),
                ("typeID", Value::Int(ty)),
            ])
        }),
        "GetMyJournalDetails" => journal(c, Some(agent.id)).await,
        "WarpToLocation" => warp_to_location(c, agent, call).await,
        "GotoLocation" | "GetEntryPoint" | "GetReplayTimestamp" | "GetDungeonShipRestrictions" => Ok(Value::None),
        "IsCheatingWithAgent" | "ShouldAlwaysAllowReplay" => Ok(Value::Bool(false)),
        "RemoveOfferFromJournal" => {
            if current.as_ref().is_some_and(|m| m.state == STATE_OFFERED) {
                if let Err(e) = remove(c, agent.id).await {
                    return Some(Err(e));
                }
                c.notify("OnAgentMissionChange", vec![Value::str("offer_removed"), Value::Int(agent.id)]);
            }
            Ok(Value::None)
        }
        "GetInfoServiceDetails" => Ok(keyval([
            ("stationID", Value::Int(agent.station_id)),
            ("level", Value::Int(agent.level)),
            ("services", Value::list([])),
            ("incompatible", Value::None),
        ])),
        _ => {
            let _ = me;
            return None;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retail_agents_load() {
        let a = agent(3009965).expect("Luminaire security agent");
        assert_eq!((a.type_id, a.level, a.station_id), (2, 1, 60012007));
    }

    #[test]
    fn epic_arc_agents_are_known() {
        // Sister Alitura (Blood-Stained Stars) and the Caldari arc's first agent.
        for id in [3019356, 3019372] {
            assert!(agent(id).is_some(), "{id}");
        }
        assert_eq!(Kind::of_template("agent.missionTemplatizedContent_EpicArcTalkToAgentMission"), Kind::Talk);
        assert_eq!(Kind::of_template("agent.missionTemplatizedContent_BasicKillMission"), Kind::Kill);
    }

    #[test]
    fn npc_classes_match_group_names() {
        assert!(group_has_class("Mission Caldari State Cruiser", "Cruiser"));
        assert!(!group_has_class("Mission Caldari State Battlecruiser", "Cruiser"));
        assert!(group_has_class("Mission Generic Battle Cruisers", "Battlecruiser"));
        assert!(group_has_class("Deadspace Guristas Frigate", "Frigate"));
        assert_eq!(spawn_classes(1).len(), 4);
    }
}
