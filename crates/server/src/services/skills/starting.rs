//! Starting skill grants for character creation, based on SDE race skills.
//! No specific client module path is established for these server-side grants.
//! Reads startup SDE race skills and type attributes and writes game.skills.
//! Other character creation is handled elsewhere.
use super::{db_err, CallError};
use super::formulas::sp_for_level;
use super::training::skill_type;
use crate::client::Client;

/// Grant the race's starting skills to a new character (sde chr_races.skills).
pub(crate) async fn grant_starting_skills(c: &Client, char_id: i64, race_id: i32) -> Result<(), CallError> {
    for skill in c.sde.characters.race(race_id as i64).into_iter().flat_map(|race| &race.skills) {
        let (type_id, level) = (skill.type_id, skill.level);
        let rank = skill_type(&c.sde, type_id).rank;
        crate::dao::skills::training::starting_skill(&c.db, char_id, type_id, level, sp_for_level(rank, level as i64)).await.map_err(db_err)?;
    }
    Ok(())
}
