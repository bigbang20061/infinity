//! Isolated database rows for atomicity tests.
use crate::db::{Db, Error};

#[derive(Clone, Copy)]
pub(in crate::services) struct Fixture { pub account: i64, pub character: i64 }

pub(in crate::services) async fn create(db: &Db) -> Result<Fixture, Error> {
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        let account = tx.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES ('atomicity-' || nextval('game.accounts_user_id_seq')::text, $1, 0) RETURNING user_id", &[&&b"fixture"[..]]).await?.get("user_id");
        let character = tx.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id) VALUES ($1, 'atomicity-' || nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1) RETURNING character_id", &[&account]).await?.get("character_id");
        Ok(Fixture { account, character })
    }), |fixture, ()| async move { Ok(fixture) }).await
}

pub(in crate::services) async fn seed_skill(db: &Db, character: i64) -> Result<(), Error> {
    db.execute("INSERT INTO game.skills (character_id, type_id, level, skill_points) VALUES ($1, 1, 0, 0)", &[&character]).await?;
    db.execute("INSERT INTO game.skill_queue (character_id, position, type_id, to_level, start_sp, dest_sp, start_time, end_time) VALUES ($1, 0, 1, 1, 0, 100, 0, 1)", &[&character]).await?;
    Ok(())
}

pub(in crate::services) async fn skill_fingerprint(db: &Db, character: i64) -> Result<String, Error> {
    Ok(db.query_one("SELECT (SELECT COALESCE(string_agg(row_to_json(s)::text, '' ORDER BY type_id), '') FROM game.skills s WHERE character_id = $1) || '|' || (SELECT COALESCE(string_agg(row_to_json(q)::text, '' ORDER BY position), '') FROM game.skill_queue q WHERE character_id = $1) AS fingerprint", &[&character]).await?.get("fingerprint"))
}

pub(in crate::services) fn assert_division_by_zero(result: Result<(), crate::services::CallError>) {
    match result {
        Err(crate::services::CallError::Internal(error)) => match error.downcast_ref::<Error>() {
            Some(Error::Postgres(error)) => assert_eq!(error.code(), Some(&tokio_postgres::error::SqlState::DIVISION_BY_ZERO)),
            _ => panic!("expected a PostgreSQL division-by-zero error"),
        },
        _ => panic!("expected a PostgreSQL division-by-zero error"),
    }
}


pub(in crate::services) async fn seed_mission(db: &Db, character: i64, agent: i64, station: i64, system: i64, mission: i32, arc: i64) -> Result<(), Error> {
    db.execute("INSERT INTO game.agent_missions (character_id, agent_id, mission_id, dungeon_id, faction_id, state, system_id, x, y, z, reward_isk, reward_lp, offered_at, kind, arc_id, cargo_type, cargo_qty) VALUES ($1, $2, $3, 0, 0, 1, $4, 0, 0, 0, 0, 0, 0, 'courier', $5, 1, 1)", &[&character, &agent, &mission, &system, &arc]).await?;
    db.execute("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) VALUES (1, $1, $2, $3, $4, false)", &[&character, &station, &crate::consts::FLAG_HANGAR, &i32::MAX]).await?;
    Ok(())
}

pub(in crate::services) async fn mission_fingerprint(db: &Db, character: i64) -> Result<String, Error> {
    Ok(db.query_one("SELECT (SELECT COALESCE(string_agg(row_to_json(m)::text, '' ORDER BY agent_id), '') FROM game.agent_missions m WHERE character_id = $1) || '|' || (SELECT COALESCE(string_agg(row_to_json(a)::text, '' ORDER BY arc_id, mission_id), '') FROM game.epic_arc_missions a WHERE character_id = $1) || '|' || (SELECT COALESCE(string_agg(row_to_json(i)::text, '' ORDER BY item_id), '') FROM game.items i WHERE owner_id = $1) AS fingerprint", &[&character]).await?.get("fingerprint"))
}
pub(in crate::services) async fn clear_cargo_stack(db: &Db, character: i64) -> Result<(), Error> {
    db.execute("DELETE FROM game.items WHERE owner_id = $1", &[&character]).await?;
    Ok(())
}


pub(in crate::services) async fn remaining(db: &Db, f: Fixture) -> Result<i64, Error> {
    Ok(db.query_one("SELECT (SELECT count(*) FROM game.accounts WHERE user_id = $1) + (SELECT count(*) FROM game.characters WHERE character_id = $2) + (SELECT count(*) FROM game.items WHERE owner_id = $2) + (SELECT count(*) FROM game.skills WHERE character_id = $2) + (SELECT count(*) FROM game.skill_queue WHERE character_id = $2) + (SELECT count(*) FROM game.agent_missions WHERE character_id = $2) + (SELECT count(*) FROM game.epic_arc_missions WHERE character_id = $2) AS remaining", &[&f.account, &f.character]).await?.get("remaining"))
}

pub(in crate::services) async fn cleanup(db: &Db, fixture: Fixture) -> Result<(), Error> {
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        tx.execute("DELETE FROM game.agent_missions WHERE character_id = $1", &[&fixture.character]).await?;
        tx.execute("DELETE FROM game.epic_arc_missions WHERE character_id = $1", &[&fixture.character]).await?;
        tx.execute("DELETE FROM game.skill_queue WHERE character_id = $1", &[&fixture.character]).await?;
        tx.execute("DELETE FROM game.skills WHERE character_id = $1", &[&fixture.character]).await?;
        tx.execute("DELETE FROM game.items WHERE owner_id = $1", &[&fixture.character]).await?;
        tx.execute("DELETE FROM game.characters WHERE character_id = $1", &[&fixture.character]).await?;
        tx.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&fixture.account]).await?;
        Ok(())
    }), |(), ()| async move { Ok(()) }).await
}
