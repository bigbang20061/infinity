//! Startup projections of mission and epic-arc data for agent services.
//! The client consumes journal messages via `evemissions.common.epic_arcs_data`.
//! Sources are `sde_native.missions` and `sde_native.epicarcs`; whole JSON objects are not loaded.

use crate::{db::{self, executor::Executor}, sde::missions::{Mission, ArcStep, JournalMessage}};
use tokio_postgres::Row;

impl Mission {
    fn from_row(row: &Row) -> Self {
        Self {
            mission_id: row.get("mission_id"), name_id: row.get("name_id"), has_name: row.get("has_name"),
            template: row.get("template"), dungeon_id: row.get("dungeon_id"), faction_id: row.get("faction_id"),
            cargo_type: row.get("cargo_type"), cargo_qty: row.get("cargo_qty"), reward_type: row.get("reward_type"),
            reward_quantity: row.get("reward_quantity"), reward_lp: row.get("reward_lp"), messages: Default::default(),
        }
    }
}
impl JournalMessage {
    fn from_row(row: &Row) -> Self {
        Self { key: row.get("key"), mission_id: row.get("mission_id"), message_id: row.get("message_id") }
    }
}
impl ArcStep {
    fn from_row(row: &Row) -> Self {
        let next: String = row.get("next_missions");
        Self {
            mission_id: row.get("mission_id"), arc: row.get("arc"), agent: row.get("agent"),
            next: next.trim_matches(|c| c == '[' || c == ']').split(',').filter_map(|n| n.trim().parse().ok()).collect(),
            fail: row.get("fail"), start: true,
        }
    }
}
pub(crate) async fn missions(ex: &(impl Executor + ?Sized)) -> Result<Vec<Mission>, db::Error> {
    Ok(ex.query("SELECT key::int AS mission_id, (data->>'nameID')::bigint AS name_id, data ? 'nameID' AS has_name,
        data->>'contentTemplate' AS template, (data->'killMission'->>'dungeonID')::int AS dungeon_id,
        (data->>'factionID')::bigint AS faction_id, (data->'courierMission'->>'objectiveTypeID')::int AS cargo_type,
        (data->'courierMission'->>'objectiveQuantity')::int AS cargo_qty,
        (data->'missionRewards'->'reward'->>'rewardTypeID')::int AS reward_type,
        (data->'missionRewards'->'reward'->>'rewardQuantity')::bigint AS reward_quantity,
        (data->>'fixedLpRewardOmega')::bigint AS reward_lp FROM sde_native.missions ORDER BY key::int", &[]).await?.iter().map(Mission::from_row).collect())
}
pub(crate) async fn messages(ex: &(impl Executor + ?Sized)) -> Result<Vec<JournalMessage>, db::Error> {
    Ok(ex.query("SELECT m.key::int AS mission_id, k AS key, (m.data->'messages'->>k)::bigint AS message_id
        FROM sde_native.missions m, jsonb_object_keys(m.data->'messages') k", &[]).await?.iter().map(JournalMessage::from_row).collect())
}
pub(crate) async fn steps(ex: &(impl Executor + ?Sized)) -> Result<Vec<ArcStep>, db::Error> {
    Ok(ex.query("SELECT e.key::bigint AS arc, m.key::int AS mission_id, COALESCE((m.value->>'agentID')::bigint, 0) AS agent,
        COALESCE(m.value->'nextMissions', '[]'::jsonb)::text AS next_missions, (m.value->>'failMissionID')::int AS fail
        FROM sde_native.epicarcs e, jsonb_each(e.data->'missions') m ORDER BY m.key::int", &[]).await?.iter().map(ArcStep::from_row).collect())
}
