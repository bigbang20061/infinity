//! Character state and mixed corporation lookups used by station and universe services.
//! Reads and writes `game.characters` and reads `sde.crp_npc_corporations` and `sde.npc_characters`.
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

pub(crate) struct NpcCorporation {
    pub name: Option<String>, pub description: Option<String>, pub ticker: Option<String>, pub tax_rate: Option<f64>,
    pub ceo_id: Option<i64>, pub station_id: Option<i64>, pub race_id: Option<i64>, pub faction_id: Option<i64>,
    pub member_limit: i64, pub member_count: i64,
}
impl NpcCorporation {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self { name: row.get("name"), description: row.get("description"), ticker: row.get("ticker"), tax_rate: row.get("tax_rate"),
            ceo_id: row.get("ceo_id"), station_id: row.get("station_id"), race_id: row.get("race_id"), faction_id: row.get("faction_id"),
            member_limit: row.get("member_limit"), member_count: row.get("member_count") }
    }
}

pub(crate) async fn npc_corporation(ex: &impl Executor, corp_id: i64, member_limit: i64) -> Result<Option<NpcCorporation>, Error> {
    Ok(ex.query_opt(
        "SELECT name, COALESCE(data->'description'->>'en', '') AS description, ticker, tax_rate, ceo_id::bigint, station_id::bigint,
                race_id::bigint, faction_id::bigint, COALESCE(member_limit::bigint, $2::bigint) AS member_limit,
                (SELECT count(*) FROM game.characters ch WHERE ch.corporation_id = $1::bigint AND NOT ch.deleted) AS member_count
         FROM sde.crp_npc_corporations WHERE corporation_id = $1::bigint",
        &[&corp_id, &member_limit],
    ).await?.as_ref().map(NpcCorporation::from_row))
}

struct CorporationId { corporation_id: Option<i64> }
impl CorporationId {
    fn from_row(row: &tokio_postgres::Row) -> Self { Self { corporation_id: row.get("corporation_id") } }
}

pub(crate) async fn corporation_for_character(ex: &impl Executor, id: i64) -> Result<Option<i64>, Error> {
    Ok(ex.query_opt(
        "SELECT corporation_id FROM game.characters WHERE character_id = $1
         UNION ALL SELECT (data->>'corporationID')::bigint FROM sde.npc_characters WHERE character_id = $1
         LIMIT 1", &[&id],
    ).await?.as_ref().and_then(|row| CorporationId::from_row(row).corporation_id))
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
