use super::*;

#[test]
fn propulsion_commands_wait_for_boundary_and_use_enqueue_stamp() {
    let mut system = System::new(0, Arc::default(), 12_345);
    let ball = Ball {
        id: 1, type_id: 1, owner_id: 1, corp_id: 1, alliance_id: -1,
        char_id: None, signature_radius: None, mode: crate::space::MODE_STOP,
        radius: 1.0, mass: 1.0, max_vel: 1.0, agility: 1.0, speed_fraction: 0.0,
        pos: [0.0; 3], vel: [0.0; 3], goto: [0.0; 3], follow_id: 0,
        old_pos: [0.0; 3], old_vel: [0.0; 3], segment_acceleration: None,
        follow_range: 0.0, free: true, global: false, massive: false, interactive: false,
        warp: crate::space::WarpState::default(), warp_factor: 0, last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0, cloak: 0,
        cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new(),
    };
    system.insert(ball);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    system.observe(1, 1, tx);
    queue_kinematics(&mut system, 1, Kinematics { speed: 20.0, mass: 30.0, agility: 0.25, signature: 40.0 });
    system.tick(12_999);
    assert_eq!(system.ball(1).unwrap().max_vel, 1.0);
    assert_eq!(system.ball(1).unwrap().agility, 1.0);
    assert!(rx.try_recv().is_err());
    system.tick(13_000);
    let ship = system.ball(1).unwrap();
    assert_eq!((ship.max_vel, ship.mass, ship.agility, ship.signature_radius), (20.0, 30.0, 0.25, Some(40.0)));
    let events = rx.try_recv().unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(events[0], event(12, "SetMaxSpeed", vec![Value::Int(1), Value::Float(20.0)]));
    assert_eq!(events[1], event(12, "SetBallMass", vec![Value::Int(1), Value::Float(30.0)]));
    assert_eq!(events[2], event(12, "SetBallAgility", vec![Value::Int(1), Value::Float(0.25)]));
    assert_eq!(system.current_time(), 13);
}
