//! Character state and corporation lookups used by station and universe services.
//! Reads and writes `game.characters`; static NPC identity is resolved by services.
//! This DAO has no client mirror and does not manage transactions or notifications.

use crate::db::{executor::Executor, Error};

struct ActivitiesPopState { state: i32 }
impl ActivitiesPopState {
    fn from_row(row: &tokio_postgres::Row) -> Self { Self { state: row.get("activities_pop_state") } }
}

pub(crate) async fn activities_pop_state(ex: &impl Executor, character_id: i64) -> Result<Option<i32>, Error> {
    Ok(ex.query_opt("SELECT activities_pop_state FROM game.characters WHERE character_id = $1", &[&character_id]).await?.as_ref().map(|row| ActivitiesPopState::from_row(row).state))
}

pub(crate) async fn set_activities_pop_state(ex: &impl Executor, character_id: i64, state: i32) -> Result<u64, Error> {
    ex.execute("UPDATE game.characters SET activities_pop_state = $2 WHERE character_id = $1", &[&character_id, &state]).await
}

struct MemberCount { member_count: i64 }
impl MemberCount {
    fn from_row(row: &tokio_postgres::Row) -> Self { Self { member_count: row.get("member_count") } }
}
pub(crate) async fn npc_member_count(ex: &impl Executor, corp_id: i64) -> Result<i64, Error> {
    let row = ex.query_one(
        "SELECT count(*) AS member_count FROM game.characters ch WHERE ch.corporation_id = $1::bigint AND NOT ch.deleted",
        &[&corp_id],
    ).await?;
    Ok(MemberCount::from_row(&row).member_count)
}

pub(crate) struct FleetMemberLocation {
    pub character_id: i64, pub solar_system_id: Option<i64>, pub station_id: Option<i64>, pub type_id: Option<i32>,
}
impl FleetMemberLocation {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self { character_id: row.get("character_id"), solar_system_id: row.get("solar_system_id"), station_id: row.get("station_id"), type_id: row.get("type_id") }
    }
}
pub(crate) async fn fleet_member_locations(ex: &impl Executor, ids: &[i64]) -> Result<Vec<FleetMemberLocation>, Error> {
    Ok(ex.query("SELECT ch.character_id, ch.solar_system_id, ch.station_id, i.type_id FROM game.characters ch LEFT JOIN game.items i ON i.item_id = ch.ship_id WHERE ch.character_id = ANY($1)", &[&ids]).await?.iter().map(FleetMemberLocation::from_row).collect())
}

struct CharacterName { name: String }
impl CharacterName {
    fn from_row(row: &tokio_postgres::Row) -> Self { Self { name: row.get("name") } }
}
pub(crate) async fn character_name(ex: &impl Executor, id: i64) -> Result<Option<String>, Error> {
    Ok(ex.query_opt("SELECT name FROM game.characters WHERE character_id = $1", &[&id]).await?.as_ref().map(|row| CharacterName::from_row(row).name))
}
