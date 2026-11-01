//! Guarded ship selection and sampled positions in `game.characters` and `game.items`.
//! The client ship-boarding services supply type and location policy; there is no wire contract here.
//! The caller holds the market lock and a transaction, and must roll it back on any refusal.
//! Static ship classification, notifications and space publication are not handled here.

use crate::consts::FLAG_HANGAR;
use crate::db::{self, executor::Executor};

/// Switch between owned positioned ships, saving both actor-sampled positions.
pub(crate) async fn board_in_space(ex: &(impl Executor + ?Sized), character: i64, old: i64, target: i64, system: i64, old_type: i32, target_type: i32, old_pos: [f64; 3], target_pos: [f64; 3]) -> Result<bool, db::Error> {
    if !old_pos.iter().chain(target_pos.iter()).all(|v| v.is_finite()) { return Ok(false); }
    board(ex, character, old, target, system, old_type, target_type, Some((old_pos, target_pos))).await
}

/// Switch between owned assembled ships in the character's station hangar.
pub(crate) async fn board_in_station(ex: &(impl Executor + ?Sized), character: i64, old: i64, target: i64, station: i64, old_type: i32, target_type: i32) -> Result<bool, db::Error> {
    board(ex, character, old, target, station, old_type, target_type, None).await
}

async fn board(ex: &(impl Executor + ?Sized), character: i64, old: i64, target: i64, location: i64, old_type: i32, target_type: i32, positions: Option<([f64; 3], [f64; 3])>) -> Result<bool, db::Error> {
    if old == target { return Ok(false); }
    let space = positions.is_some();
    let flag = if space { 0 } else { FLAG_HANGAR };
    if ex.query_opt("SELECT character_id FROM game.characters WHERE character_id=$1 AND ship_id=$2 AND NOT deleted AND (($3 AND station_id IS NULL AND solar_system_id=$4) OR (NOT $3 AND station_id=$4)) FOR UPDATE", &[&character, &old, &space, &location]).await?.is_none() { return Ok(false); }
    let ids = [old, target];
    let rows = ex.query("SELECT item_id FROM game.items WHERE item_id=ANY($1) ORDER BY item_id FOR UPDATE", &[&&ids[..]]).await?;
    if rows.len() != 2 { return Ok(false); }
    for (id, kind) in [(old, old_type), (target, target_type)] {
        if ex.query_opt("SELECT item_id FROM game.items WHERE item_id=$1 AND type_id=$2 AND owner_id=$3 AND location_id=$4 AND flag_id=$5 AND singleton AND quantity=1 AND (NOT $6 OR (x > '-Infinity'::float8 AND x < 'Infinity'::float8 AND y > '-Infinity'::float8 AND y < 'Infinity'::float8 AND z > '-Infinity'::float8 AND z < 'Infinity'::float8))", &[&id, &kind, &character, &location, &flag, &space]).await?.is_none() { return Ok(false); }
    }
    if ex.query_opt("SELECT character_id FROM game.characters WHERE ship_id=$1 LIMIT 1", &[&target]).await?.is_some() { return Ok(false); }
    if let Some((old_pos, target_pos)) = positions {
        let mut writes = [(old, old_type, old_pos), (target, target_type, target_pos)];
        writes.sort_unstable_by_key(|entry| entry.0);
        for (id, kind, pos) in writes {
            if ex.execute("UPDATE game.items SET x=$6,y=$7,z=$8 WHERE item_id=$1 AND type_id=$2 AND owner_id=$3 AND location_id=$4 AND flag_id=$5 AND singleton AND quantity=1 AND x > '-Infinity'::float8 AND x < 'Infinity'::float8 AND y > '-Infinity'::float8 AND y < 'Infinity'::float8 AND z > '-Infinity'::float8 AND z < 'Infinity'::float8", &[&id, &kind, &character, &location, &flag, &pos[0], &pos[1], &pos[2]]).await? != 1 { return Ok(false); }
        }
    }
    Ok(ex.execute("UPDATE game.characters SET ship_id=$3 WHERE character_id=$1 AND ship_id=$2 AND NOT deleted AND (($4 AND station_id IS NULL AND solar_system_id=$5) OR (NOT $4 AND station_id=$5)) AND NOT EXISTS (SELECT 1 FROM game.characters selected WHERE selected.ship_id=$3)", &[&character, &old, &target, &space, &location]).await? == 1)
}

#[cfg(test)] mod tests;
