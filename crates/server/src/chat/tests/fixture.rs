//! Isolated characters and live game clients for chat socket tests.
use std::sync::Arc;
use crate::{client::Client, db::Db, chat::Hub, sde::Sde, space::World};
use tokio::net::{TcpListener, TcpStream};

#[derive(Clone, Copy)]
pub(super) struct Character { pub account: i64, pub id: i64 }

pub(super) async fn create(db: &Db) -> Character {
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        let account = tx.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES ('chat-test-' || nextval('game.accounts_user_id_seq')::text, $1, 0) RETURNING user_id", &[&&b"fixture"[..]]).await?.get("user_id");
        let id = tx.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id) VALUES ($1, 'chat-test-' || nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1) RETURNING character_id", &[&account]).await?.get("character_id");
        Ok::<_, crate::db::Error>(Character { account, id })
    }), |v, ()| async move { Ok(v) }).await.unwrap()
}

pub(super) async fn cleanup(db: &Db, chars: &[Character]) {
    let ids: Vec<i64> = chars.iter().map(|c| c.id).collect();
    db.execute("DELETE FROM game.chat_channels WHERE owner_id = ANY($1)", &[&ids]).await.unwrap();
    for c in chars {
        db.execute("DELETE FROM game.characters WHERE character_id = $1", &[&c.id]).await.unwrap();
        db.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&c.account]).await.unwrap();
    }
}

pub(super) async fn client(db: Db, sde: Arc<Sde>, world: Arc<World>, hub: Arc<Hub>) -> (Client, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let (stream, address) = listener.accept().await.unwrap();
    let cfg = Arc::new(crate::config::Config::default());
    let login = crate::handshake::Login { user_id: 1, user_name: "test".into(), role: 0, client_id: 1, session_id: 1, language_id: "EN".into() };
    (Client::new(crate::conn::Conn::new(stream, address), cfg, db, sde, hub, world, login), peer)
}
