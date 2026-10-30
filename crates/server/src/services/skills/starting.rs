//! Starting skill grants for character creation, based on SDE race skills.
//! No specific client module path is established for these server-side grants.
//! Reads sde.chr_races and sde.type_attributes and writes game.skills.
//! Other character creation is handled elsewhere.
use super::{db_err, CallError};
use super::formulas::sp_for_level;
use super::training::skill_type;
use crate::client::Client;

/// Grant the race's starting skills to a new character (sde chr_races.skills).
pub(crate) async fn grant_starting_skills(c: &Client, char_id: i64, race_id: i32) -> Result<(), CallError> {
    let rows = c
        .db
        .query(
            "SELECT (s->>'_key')::int AS type_id, (s->>'_value')::int AS level FROM sde.chr_races r, jsonb_array_elements(r.data->'skills') s WHERE r.race_id = $1",
            &[&race_id],
        )
        .await
        .map_err(db_err)?;
    for r in rows {
        let (type_id, level): (i32, i32) = (r.get("type_id"), r.get("level"));
        let rank = skill_type(&c.db, type_id).await?.rank;
        crate::dao::skills::training::starting_skill(&c.db, char_id, type_id, level, sp_for_level(rank, level as i64)).await.map_err(db_err)?;
    }
    Ok(())
}
