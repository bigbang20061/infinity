//! Named rows for the mission service's static-data queries.
//! The queries themselves remain alongside their service mechanics.

use tokio_postgres::Row;

pub(super) struct Message { pub id: Option<i64> }
impl Message { pub(super) fn from_row(r: &Row) -> Self { Self { id: r.get("message_id") } } }
pub(super) struct Pick { pub mission_id: i32, pub dungeon_id: i32, pub faction_id: i64 }
impl Pick { pub(super) fn from_row(r: &Row) -> Self { Self { mission_id: r.get("mission_id"), dungeon_id: r.get("dungeon_id"), faction_id: r.get("faction_id") } } }
pub(super) struct ArcStepRow { pub arc_id: i64, pub mission_id: i32, pub agent_id: Option<i64>, pub next: String, pub fail: Option<i32> }
impl ArcStepRow { pub(super) fn from_row(r: &Row) -> Self { Self { arc_id: r.get("arc_id"), mission_id: r.get("mission_id"), agent_id: r.get("agent_id"), next: r.get("next_missions"), fail: r.get("fail_mission_id") } } }
pub(super) struct ArcOfferRow { pub template: Option<String>, pub dungeon_id: i32, pub faction_id: i64, pub cargo_type: Option<i32>, pub cargo_qty: Option<i32>, pub reward_isk: i64, pub reward_lp: i64 }
impl ArcOfferRow { pub(super) fn from_row(r: &Row) -> Self { Self { template: r.get("template"), dungeon_id: r.get("dungeon_id"), faction_id: r.get("faction_id"), cargo_type: r.get("cargo_type"), cargo_qty: r.get("cargo_qty"), reward_isk: r.get("reward_isk"), reward_lp: r.get("reward_lp") } } }
pub(super) struct Reward { pub type_id: Option<i32>, pub quantity: Option<i32> }
impl Reward { pub(super) fn from_row(r: &Row) -> Self { Self { type_id: r.get("type_id"), quantity: r.get("quantity") } } }
pub(super) struct Npc { pub type_id: i32, pub name: String, pub radius: f64, pub mass: f64, pub velocity: Option<f64>, pub agility: Option<f64>, pub generic: bool }
impl Npc { pub(super) fn from_row(r: &Row) -> Self { Self { type_id: r.get("type_id"), name: r.get("name"), radius: r.get("radius"), mass: r.get("mass"), velocity: r.get("velocity"), agility: r.get("agility"), generic: r.get("generic") } } }
pub(super) struct Corporation { pub id: Option<i64> }
impl Corporation { pub(super) fn from_row(r: &Row) -> Self { Self { id: r.get("corporation_id") } } }
pub(super) struct Station { pub type_id: i64, pub system_id: i64, pub x: f64, pub y: f64, pub z: f64 }
impl Station { pub(super) fn from_row(r: &Row) -> Self { Self { type_id: r.get("type_id"), system_id: r.get("system_id"), x: r.get("x"), y: r.get("y"), z: r.get("z") } } }
pub(super) struct Volume { pub value: f64 }
impl Volume { pub(super) fn from_row(r: &Row) -> Self { Self { value: r.get("volume") } } }
