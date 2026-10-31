use super::*;

#[test]
fn unix_milliseconds_convert_to_filetime() {
    assert_eq!(filetime_from_unix_ms(0), FILETIME_UNIX_EPOCH);
    assert_eq!(filetime_from_unix_ms(1), FILETIME_UNIX_EPOCH + 10_000);
    assert_eq!(filetime_from_unix_ms(-1), FILETIME_UNIX_EPOCH - 10_000);
}

#[test]
fn signed_filetime_seconds_conversions() {
    assert_eq!(unix_seconds_from_filetime(FILETIME_UNIX_EPOCH + FILETIME_SECOND + 1), 1);
    assert_eq!(unix_seconds_from_filetime(FILETIME_UNIX_EPOCH - FILETIME_SECOND - 1), -1);
    assert_eq!(unix_seconds_from_filetime(FILETIME_UNIX_EPOCH - 1), 0);
    assert_eq!(filetime_ticks_from_seconds(2), 2 * FILETIME_SECOND);
    assert_eq!(filetime_ticks_from_seconds(-2), -2 * FILETIME_SECOND);
    assert_eq!(filetime_ticks_from_seconds(0), 0);
}

#[test]
fn filetime_timestamp_milliseconds_round_up() {
    assert_eq!(unix_ms_from_filetime_ceil(FILETIME_UNIX_EPOCH), 0);
    assert_eq!(unix_ms_from_filetime_ceil(FILETIME_UNIX_EPOCH + FILETIME_TICKS_PER_MS), 1);
    assert_eq!(unix_ms_from_filetime_ceil(FILETIME_UNIX_EPOCH + FILETIME_TICKS_PER_MS + 1), 2);
    assert_eq!(unix_ms_from_filetime_ceil(FILETIME_UNIX_EPOCH - FILETIME_TICKS_PER_MS), -1);
    assert_eq!(unix_ms_from_filetime_ceil(FILETIME_UNIX_EPOCH - FILETIME_TICKS_PER_MS + 1), 0);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn wait_flushes_queued_notifications_and_forwards_later_destiny() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let world = crate::space::World::new(db.clone(), sde.clone(), 1);
    let system = -1;
    world.system(system).await.unwrap();
    let cfg = Arc::new(Config::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let (stream, address) = listener.accept().await.unwrap();
    let login = Login { user_id: 1, user_name: "test".into(), role: 0, client_id: 1, session_id: 1, language_id: "EN".into() };
    let chat = crate::chat::Hub::new(&cfg.chat.domain);
    let mut client = Client::new(Conn::new(stream, address), cfg, db, sde, chat, world, login);
    client.notify("OnDockingAccepted", vec![Value::Int(42)]);
    let tx = client.destiny_tx.clone();
    let wait = client.wait_pumping(system, 2000);
    tokio::pin!(wait);
    let first = {
        let read_name = async {
            let frame = crate::framing::read_frame(&mut peer).await.unwrap().unwrap();
            let packet = Packet::from_value(&marshal::loads(&frame.payload).unwrap()).unwrap();
            match packet.destination {
                Address::Broadcast { broadcast_id, .. } => broadcast_id.as_text().unwrap().into_owned(),
                _ => panic!("expected broadcast"),
            }
        };
        tokio::pin!(read_name);
        tokio::select! {
            _ = &mut wait => panic!("timer completed before queued notification"),
            name = &mut read_name => name,
        }
    };
    assert_eq!(first, "OnDockingAccepted");
    tx.send(vec![Value::Int(99)]).unwrap();
    let second = tokio::select! {
        _ = &mut wait => panic!("timer completed before delayed destiny update"),
        frame = crate::framing::read_frame(&mut peer) => frame.unwrap().unwrap(),
    };
    let packet = Packet::from_value(&marshal::loads(&second.payload).unwrap()).unwrap();
    assert!(matches!(packet.destination, Address::Broadcast { broadcast_id, .. } if broadcast_id.as_text().as_deref() == Some("DoDestinyUpdate")));
    assert!(wait.await.unwrap());
}
