use super::*;
use std::sync::Arc;

pub(super) mod fixture;

pub(super) fn database_message(error: &anyhow::Error) -> Option<&str> {
    match error.downcast_ref::<crate::db::Error>()? {
        crate::db::Error::Postgres(error) => error.as_db_error().map(|error| error.message()),
        _ => None,
    }
}

pub(super) async fn client(db: crate::db::Db, character: i64) -> (Client, tokio::net::TcpStream) {
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let world = crate::space::World::new(db.clone(), sde.clone(), 1);
    let cfg = Arc::new(crate::config::Config::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let (stream, address) = listener.accept().await.unwrap();
    let conn = crate::conn::Conn::new(stream, address);
    let login = crate::handshake::Login { user_id: 1, user_name: "test".into(), role: 0, client_id: 1, session_id: 1, language_id: "EN".into() };
    let chat = crate::chat::Hub::new(&cfg.chat.domain);
    let mut client = Client::new(conn, cfg, db, sde, chat, world, login);
    client.session.apply(vec![("charid", Value::Int(character))]);
    (client, peer)
}

#[test]
fn surface_offset_includes_radius() {
    let center = [1.0e9, -2.0e9, 3.0e9];
    for radius in [0.0, 6_000_000.0, 1.0e12] {
        let clearance = 1_000_000.0;
        let actual = crate::space::dist(center, off_surface(center, radius, clearance));
        let expected = radius + clearance;
        assert!((actual - expected).abs() < expected * 1e-12, "{actual} != {expected}");
    }
}
