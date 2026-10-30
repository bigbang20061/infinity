//! Agent mission and epic arc progress persistence, including mission payments.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Uses `game.agent_missions`, `game.epic_arc_missions`, `game.characters` and
//! `sde_native.missions`. Static-data-only lookups, mission mechanics, transactions
//! and notifications remain with callers.

use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

const MISSION_COLUMNS: &str = "am.agent_id, am.mission_id, (m.data->>'nameID')::bigint AS name_id, (m.data->'messages'->>'messages.mission.briefing')::bigint AS briefing_id,
    am.dungeon_id, am.faction_id, am.state, am.system_id, am.x, am.y, am.z, am.reward_isk, am.reward_lp, am.accepted_at,
    am.kind, am.arc_id, am.dest_agent, am.cargo_type, am.cargo_qty";

pub(crate) struct MissionRow {
    pub agent_id: i64, pub mission_id: i32, pub name_id: Option<i64>, pub briefing_id: Option<i64>,
    pub dungeon_id: i32, pub faction_id: i64, pub state: i32, pub system_id: i64,
    pub x: f64, pub y: f64, pub z: f64, pub reward_isk: i64, pub reward_lp: i64,
    pub accepted_at: Option<i64>, pub kind: String, pub arc_id: Option<i64>, pub dest_agent: Option<i64>,
    pub cargo_type: Option<i32>, pub cargo_qty: Option<i32>,
}
impl MissionRow {
    fn from_row(r: &Row) -> Self {
        Self { agent_id: r.get("agent_id"), mission_id: r.get("mission_id"), name_id: r.get("name_id"),
            briefing_id: r.get("briefing_id"), dungeon_id: r.get("dungeon_id"), faction_id: r.get("faction_id"),
            state: r.get("state"), system_id: r.get("system_id"), x: r.get("x"), y: r.get("y"), z: r.get("z"),
            reward_isk: r.get("reward_isk"), reward_lp: r.get("reward_lp"), accepted_at: r.get("accepted_at"),
            kind: r.get("kind"), arc_id: r.get("arc_id"), dest_agent: r.get("dest_agent"),
            cargo_type: r.get("cargo_type"), cargo_qty: r.get("cargo_qty") }
    }
}
pub(crate) async fn missions_of(ex: &(impl Executor + ?Sized), character_id: i64, agent_id: Option<i64>) -> Result<Vec<MissionRow>, db::Error> {
    let rows = ex.query(&format!("SELECT {MISSION_COLUMNS} FROM game.agent_missions am JOIN sde_native.missions m ON m.key = am.mission_id::text
                 WHERE am.character_id = $1 AND ($2::bigint IS NULL OR am.agent_id = $2) ORDER BY am.offered_at"), &[&character_id, &agent_id]).await?;
    Ok(rows.iter().map(MissionRow::from_row).collect())
}

pub(crate) struct ArcProgress { pub arc_id: i64, pub mission_id: i32, pub state: String }
impl ArcProgress { fn from_row(r: &Row) -> Self { Self { arc_id: r.get("arc_id"), mission_id: r.get("mission_id"), state: r.get("state") } } }
pub(crate) async fn arc_progress(ex: &(impl Executor + ?Sized), character_id: i64) -> Result<Vec<ArcProgress>, db::Error> {
    Ok(ex.query("SELECT arc_id, mission_id, state FROM game.epic_arc_missions WHERE character_id = $1", &[&character_id]).await?.iter().map(ArcProgress::from_row).collect())
}
pub(crate) async fn set_arc_state(ex: &(impl Executor + ?Sized), character_id: i64, arc: i64, mission: i32, state: &str, stamp: Option<i64>) -> Result<u64, db::Error> {
    let column = match state { "accepted" => "accepted_at", "completed" => "completed_at", "quit" => "quit_at", _ => "accepted_at" };
    ex.execute(&format!("INSERT INTO game.epic_arc_missions (character_id, arc_id, mission_id, state, {column}) VALUES ($1, $2, $3, $4, $5)
                 ON CONFLICT (character_id, arc_id, mission_id) DO UPDATE SET state = EXCLUDED.state, {column} = COALESCE(EXCLUDED.{column}, game.epic_arc_missions.{column})"), &[&character_id, &arc, &mission, &state, &stamp]).await
}

pub(crate) struct ArcStatus { pub arc_id: i64, pub mission_id: i32, pub accepted_at: Option<i64>, pub completed_at: Option<i64>, pub quit_at: Option<i64>, pub name_id: Option<i64> }
impl ArcStatus { fn from_row(r: &Row) -> Self { Self { arc_id: r.get("arc_id"), mission_id: r.get("mission_id"), accepted_at: r.get("accepted_at"), completed_at: r.get("completed_at"), quit_at: r.get("quit_at"), name_id: r.get("name_id") } } }
pub(crate) async fn arc_status(ex: &(impl Executor + ?Sized), character_id: i64) -> Result<Vec<ArcStatus>, db::Error> {
    Ok(ex.query("SELECT e.arc_id, e.mission_id, e.accepted_at, e.completed_at, e.quit_at, (m.data->>'nameID')::bigint AS name_id
             FROM game.epic_arc_missions e JOIN sde_native.missions m ON m.key = e.mission_id::text
             WHERE e.character_id = $1 AND e.state <> 'unlocked' ORDER BY e.arc_id", &[&character_id]).await?.iter().map(ArcStatus::from_row).collect())
}

pub(crate) struct Offer { pub character_id: i64, pub agent_id: i64, pub mission_id: i32, pub dungeon_id: i32, pub faction_id: i64, pub state: i32, pub system_id: i64, pub pos: [f64; 3], pub reward_isk: i64, pub reward_lp: i64, pub offered_at: i64 }
pub(crate) struct ArcOffer { pub offer: Offer, pub kind: &'static str, pub arc_id: i64, pub dest_agent: Option<i64>, pub cargo_type: Option<i32>, pub cargo_qty: Option<i32> }
pub(crate) async fn offer(ex: &(impl Executor + ?Sized), o: &Offer) -> Result<u64, db::Error> {
    ex.execute("INSERT INTO game.agent_missions (character_id, agent_id, mission_id, dungeon_id, faction_id, state, system_id, x, y, z, reward_isk, reward_lp, offered_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
             ON CONFLICT (character_id, agent_id) DO NOTHING", &[&o.character_id, &o.agent_id, &o.mission_id, &o.dungeon_id, &o.faction_id, &o.state, &o.system_id, &o.pos[0], &o.pos[1], &o.pos[2], &o.reward_isk, &o.reward_lp, &o.offered_at]).await
}
pub(crate) async fn offer_arc(ex: &(impl Executor + ?Sized), a: &ArcOffer) -> Result<u64, db::Error> {
    let o = &a.offer;
    ex.execute("INSERT INTO game.agent_missions (character_id, agent_id, mission_id, dungeon_id, faction_id, state, system_id, x, y, z,
                 reward_isk, reward_lp, offered_at, kind, arc_id, dest_agent, cargo_type, cargo_qty)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18)
             ON CONFLICT (character_id, agent_id) DO NOTHING", &[&o.character_id, &o.agent_id, &o.mission_id, &o.dungeon_id, &o.faction_id, &o.state, &o.system_id, &o.pos[0], &o.pos[1], &o.pos[2], &o.reward_isk, &o.reward_lp, &o.offered_at, &a.kind, &a.arc_id, &a.dest_agent, &a.cargo_type, &a.cargo_qty]).await
}
pub(crate) async fn set_state(ex: &(impl Executor + ?Sized), character_id: i64, agent_id: i64, state: i32, stamp: i64) -> Result<u64, db::Error> {
    ex.execute("UPDATE game.agent_missions SET state = $3, accepted_at = CASE WHEN $3 = 2 THEN $4 ELSE accepted_at END
             WHERE character_id = $1 AND agent_id = $2", &[&character_id, &agent_id, &state, &stamp]).await
}
pub(crate) async fn accept_offered(ex: &(impl Executor + ?Sized), character_id: i64, agent_id: i64, mission_id: i32, offered: i32, accepted: i32, stamp: i64) -> Result<u64, db::Error> {
    ex.execute("UPDATE game.agent_missions SET state = $5, accepted_at = $6 WHERE character_id = $1 AND agent_id = $2 AND mission_id = $3 AND state = $4",
        &[&character_id, &agent_id, &mission_id, &offered, &accepted, &stamp]).await
}
pub(crate) async fn remove(ex: &(impl Executor + ?Sized), character_id: i64, agent_id: i64) -> Result<u64, db::Error> {
    ex.execute("DELETE FROM game.agent_missions WHERE character_id = $1 AND agent_id = $2", &[&character_id, &agent_id]).await
}
pub(crate) async fn remove_accepted(ex: &(impl Executor + ?Sized), character_id: i64, agent_id: i64, mission_id: i32, state: i32) -> Result<u64, db::Error> {
    ex.execute("DELETE FROM game.agent_missions WHERE character_id = $1 AND agent_id = $2 AND mission_id = $3 AND state = $4", &[&character_id, &agent_id, &mission_id, &state]).await
}
pub(crate) struct Balance { pub balance: f64 }
impl Balance { fn from_row(r: &Row) -> Self { Self { balance: r.get("balance") } } }
pub(crate) async fn pay(ex: &(impl Executor + ?Sized), character_id: i64, amount: i64) -> Result<Balance, db::Error> {
    Ok(Balance::from_row(&ex.query_one("UPDATE game.characters SET balance = balance + $2::bigint WHERE character_id = $1 RETURNING balance::float8 AS balance", &[&character_id, &amount]).await?))
}
