//! Test-only isolated game rows for Lua mutation integration tests.
use crate::db::{Db, Error};

#[derive(Clone, Copy)]
pub(super) struct Fixture { pub account: i64, pub chars: [i64; 3] }

pub(super) async fn create(db: &Db) -> Result<Fixture, Error> {
    db.transaction(
        |tx, _: &mut ()| Box::pin(async move {
            let account = tx.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES ('lua-fixture-' || nextval('game.accounts_user_id_seq')::text, $1, 0) RETURNING user_id", &[&&b"fixture"[..]]).await?.get("user_id");
            let mut chars = [0; 3];
            for id in &mut chars {
                *id = tx.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id) VALUES ($1, 'lua-fixture-' || nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1) RETURNING character_id", &[&account]).await?.get("character_id");
            }
            Ok(Fixture { account, chars })
        }),
        |fixture, ()| async move { Ok(fixture) },
    ).await
}

pub(super) async fn cleanup(db: &Db, fixture: Fixture) -> Result<(), Error> {
    db.transaction(
        |tx, _: &mut ()| Box::pin(async move {
            tx.execute("DELETE FROM game.mail_status WHERE message_id IN (SELECT message_id FROM game.mail_messages WHERE sender_id = ANY($1))", &[&&fixture.chars[..]]).await?;
            tx.execute("DELETE FROM game.mail_messages WHERE sender_id = ANY($1)", &[&&fixture.chars[..]]).await?;
            tx.execute("DELETE FROM game.standings WHERE char_id = ANY($1)", &[&&fixture.chars[..]]).await?;
            tx.execute("DELETE FROM game.items WHERE owner_id = ANY($1)", &[&&fixture.chars[..]]).await?;
            tx.execute("DELETE FROM game.characters WHERE user_id = $1", &[&fixture.account]).await?;
            tx.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&fixture.account]).await?;
            Ok(())
        }),
        |(), ()| async move { Ok(()) },
    ).await
}

pub(super) async fn fingerprint(db: &Db) -> Result<Vec<String>, Error> {
    let mut fingerprints = Vec::new();
    for query in [
        "SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY user_id), '')) AS fingerprint FROM game.accounts t",
        "SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY character_id), '')) AS fingerprint FROM game.characters t",
        "SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY item_id), '')) AS fingerprint FROM game.items t",
        "SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY message_id), '')) AS fingerprint FROM game.mail_messages t",
        "SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY char_id, message_id), '')) AS fingerprint FROM game.mail_status t",
        "SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY char_id, from_id), '')) AS fingerprint FROM game.standings t",
    ] { fingerprints.push(db.query_one(query, &[]).await?.get("fingerprint")); }
    Ok(fingerprints)
}

pub(super) async fn hold_character(
    db: &Db,
    character: i64,
    ready: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
) -> Result<(), Error> {
    let state = std::sync::Arc::new(tokio::sync::Mutex::new(Some((ready, release))));
    db.transaction(
        |tx, _: &mut ()| {
            let state = state.clone();
            Box::pin(async move {
                tx.query_one("SELECT character_id FROM game.characters WHERE character_id = $1 FOR UPDATE", &[&character]).await?;
                let (ready, release) = state.lock().await.take().expect("fixture lock transaction retried");
                let _ = ready.send(());
                let _ = release.await;
                Ok(())
            })
        },
        |(), ()| async move { Ok(()) },
    ).await
}

pub(super) async fn wallet_update_waiting(db: &Db) -> Result<bool, Error> {
    let row = db.query_one(
        "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE datname = current_database() AND wait_event_type = 'Lock' AND query LIKE 'UPDATE game.characters SET balance = balance +%') AS waiting",
        &[],
    ).await?;
    Ok(row.get("waiting"))
}
