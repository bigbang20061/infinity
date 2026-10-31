use super::*;
use crate::space::{MODE_GOTO, MODE_WARP};

fn ship() -> Ball {
    let mut b = Ball {
        id: 123, type_id: 456, owner_id: 789, corp_id: 12, alliance_id: 34,
        char_id: None, mode: MODE_GOTO, radius: 50.0, signature_radius: None,
        mass: 1_000_000.0, max_vel: 200.0, agility: 3.0, speed_fraction: 0.0,
        pos: [10.0, 20.0, 30.0], vel: [4.0, 5.0, 6.0],
        old_pos: [1.0, 2.0, 3.0], old_vel: [7.0, 8.0, 9.0],
        segment_acceleration: Some([11.0, 12.0, 13.0]),
        goto: [0.0; 3], follow_id: 0, follow_range: 0.0,
        free: true, global: false, massive: false, interactive: false,
        warp: crate::space::WarpState::default(),
        warp_factor: 3000, last_ms: 1000, interpolation_anchor_ms: 500, effect_stamp: 42,
        cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: vec![],
    };
    b.warp.length = 20.0;
    b.warp.min_range = 10.0;
    b.warp.factor = 3;
    b
}

#[test]
fn property_setters_preserve_retained_warp_metadata() {
    let mut b = ship();
    b.mode = MODE_WARP;
    b.set_speed_fraction(0.5);
    b.set_max_speed(250.0);
    b.set_ball_mass(2_000_000.0);
    assert_eq!(b.mode, MODE_WARP);
    assert!(b.is_warping());
    assert_eq!((b.warp.length, b.warp.min_range, b.warp.factor), (20.0, 10.0, 3));
    assert_eq!(b.warp_factor, 3000);
    assert_eq!(b.effect_stamp, 42);
    assert_eq!(b.segment_acceleration, Some([11.0, 12.0, 13.0]));
}

#[test]
fn short_warp_uses_raw_point_and_preserves_motion_state() {
    let point = [100_009.0, 20.0, 30.0];
    for (object, min_range, throttle) in [
        (None, -5000.0, 0.0),
        (None, 5000.0, 0.4),
        (Some((point, 10_000.0)), 5000.0, 0.4),
    ] {
        let mut b = ship();
        b.speed_fraction = throttle;
        let events = warp_command(&mut b, 77, object, point, min_range);
        assert_eq!(b.mode, MODE_GOTO);
        assert_eq!(b.goto, point);
        assert!(!b.is_warping());
        assert_eq!((b.warp.length, b.warp.min_range, b.warp.factor), (20.0, 10.0, 3));
        assert_eq!(b.speed_fraction, if throttle == 0.0 { 1.0 } else { throttle });
        assert_eq!(b.pos, [10.0, 20.0, 30.0]);
        assert_eq!(b.vel, [4.0, 5.0, 6.0]);
        assert_eq!(b.old_pos, [1.0, 2.0, 3.0]);
        assert_eq!(b.old_vel, [7.0, 8.0, 9.0]);
        assert_eq!(b.segment_acceleration, Some([11.0, 12.0, 13.0]));
        assert_eq!(b.last_ms, 1000);
        assert_eq!(b.interpolation_anchor_ms, 500);
        assert_eq!(b.effect_stamp, 42);
        assert_eq!(events, vec![event(77, "GotoPoint", vec![Value::Int(123), Value::Float(point[0]), Value::Float(point[1]), Value::Float(point[2])])]);
    }
}

#[test]
fn warp_threshold_uses_unadjusted_distance_and_keeps_long_warp_events() {
    for (distance, min_range) in [(100_000.0, 0.0), (100_001.0, 5000.0)] {
        let mut b = ship();
        let point = [b.pos[0] + distance, b.pos[1], b.pos[2]];
        let events = warp_command(&mut b, 77, None, point, min_range);
        assert_eq!(b.mode, MODE_WARP);
        assert_eq!((b.warp.length, b.warp.min_range, b.warp.factor), (20.0, min_range, 3000));
        assert_eq!(b.goto, point);
        assert_eq!(events, vec![
            event(77, "WarpTo", vec![Value::Int(123), Value::Float(point[0]), Value::Float(point[1]), Value::Float(point[2]), Value::Float(min_range), Value::Int(3000)]),
            event(77, "SetSpeedFraction", vec![Value::Int(123), Value::Float(1.0)]),
            special_fx(77, 123, None, None, "effects.Warping", 1, 0, SPECIAL_FX_UNSPECIFIED_DURATION, None),
        ]);
    }
}

#[test]
fn stale_warp_snapshot_has_no_side_effects_and_can_retry() {
    let mut s = space::System::new(0, std::sync::Arc::default(), 12_345);
    let mut b = ship();
    b.cloak = 1;
    s.insert(b);
    let old = s.projection_revision(123);
    s.refresh_projection_target(123);
    let point = [200_000.0, 20.0, 30.0];
    assert!(!commit_warp(&mut s, 123, old, false, None, point, 0.0, None).0);
    assert_eq!(s.ball(123).unwrap().cloak, 1);
    s.tick(13_000);
    assert_ne!(s.ball(123).unwrap().mode, MODE_WARP);
    let current = s.projection_revision(123);
    assert!(commit_warp(&mut s, 123, current, true, None, point, 0.0, None).0);
    assert_eq!(s.ball(123).unwrap().cloak, 1);
    s.refresh_projection_target(123);
    assert!(!commit_warp(&mut s, 123, current, true, None, point, 0.0, None).0);
    let current = s.projection_revision(123);
    assert!(commit_warp(&mut s, 123, current, false, None, point, 0.0, None).0);
    s.tick(14_000);
    assert_eq!(s.ball(123).unwrap().mode, MODE_WARP);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn docking_checks_surface_range_before_accepting() {
    use std::sync::Arc;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let world = space::World::new(db.clone(), sde.clone(), 1);
    let system = -1;
    world.system(system).await.unwrap();
    let cfg = Arc::new(crate::config::Config::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let (stream, address) = listener.accept().await.unwrap();
    let login = crate::handshake::Login { user_id: 1, user_name: "test".into(), role: 0, client_id: 1, session_id: 1, language_id: "EN".into() };
    let chat = crate::chat::Hub::new(&cfg.chat.domain);
    let mut client = Client::new(crate::conn::Conn::new(stream, address), cfg, db, sde, chat, world.clone(), login);
    client.session.apply(vec![("solarsystemid", Value::Int(system)), ("shipid", Value::Int(123))]);
    let mut vessel = ship();
    vessel.mode = MODE_STOP;
    vessel.pos = [0.0; 3];
    vessel.old_pos = vessel.pos;
    vessel.vel = [0.0; 3];
    vessel.old_vel = vessel.vel;
    vessel.segment_acceleration = None;
    let mut station = vessel.clone();
    station.id = 456;
    station.radius = 1000.0;
    station.pos = [3551.0, 0.0, 0.0];
    station.old_pos = station.pos;
    world.with(system, move |s| { s.insert(vessel); s.insert(station); }).await.unwrap();
    for target in [999, 456] {
        assert!(matches!(dock(&mut client, target).await, Err(CallError::User { msg, .. }) if msg == messages::MSG_DOCKING_REQUEST_DENIED));
    }
    world.with(system, |s| {
        let mut station = s.ball(456).unwrap().clone();
        station.pos = [3550.0, 0.0, 0.0];
        station.old_pos = station.pos;
        s.insert(station);
    }).await.unwrap();
    let docking = dock(&mut client, 456);
    tokio::pin!(docking);
    let frame = tokio::select! {
        _ = &mut docking => panic!("docking finalized before acceptance delay"),
        frame = tokio::time::timeout(std::time::Duration::from_secs(2), crate::framing::read_frame(&mut peer)) => frame.unwrap().unwrap().unwrap(),
    };
    let packet = crate::macho::Packet::from_value(&marshal::loads(&frame.payload).unwrap()).unwrap();
    assert!(matches!(packet.destination, crate::macho::Address::Broadcast { broadcast_id, .. } if broadcast_id.as_text().as_deref() == Some("OnDockingAccepted")));
}
