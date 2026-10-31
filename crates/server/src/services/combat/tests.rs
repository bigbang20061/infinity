use super::*;
use crate::space::{event, System};

#[test]
fn npc_missile_uses_both_effective_multipliers() {
    assert_eq!(npc_missile_kinematics(3000.0, 5000.0, 1.0, 1.0), (3000.0, 15000.0));
    assert_eq!(npc_missile_kinematics(3000.0, 5000.0, 0.5, 0.5), (1500.0, 3750.0));
}

#[test]
fn reload_reservations_share_stacks_in_order_without_overdraw() {
    let item = |id, type_id, quantity| crate::dao::items::loading::Item {
        item_id: id, type_id, owner_id: 1, location_id: 2, flag_id: FLAG_CARGO,
        quantity, singleton: false, group_id: 0, category_id: 0, custom_info: None,
    };
    let mut items = BTreeMap::from([
        (1, (item(1, 10, 3), 3)),
        (2, (item(2, 10, 4), 4)),
        (3, (item(3, 11, 5), 5)),
    ]);
    let selected = [1, 2, 3];
    assert_eq!(reserve_quantities(&mut items, &selected, 10, 5), 0);
    assert_eq!((items[&1].1, items[&2].1, items[&3].1), (0, 2, 5));
    assert_eq!(reserve_quantities(&mut items, &selected, 10, 4), 2);
    assert_eq!((items[&1].1, items[&2].1, items[&3].1), (0, 0, 5));
    assert_eq!(reserve_quantities(&mut items, &selected, 11, 3), 0);
    assert_eq!(items[&3].1, 2);
}
#[test]
fn module_ids_preserve_scalar_and_list_shapes() {
    assert_eq!(module_ids(None), Vec::<i64>::new());
    assert_eq!(module_ids(Some(&Value::Int(42))), vec![42]);
    assert_eq!(module_ids(Some(&Value::Bool(true))), vec![1]);
    assert_eq!(module_ids(Some(&Value::Bool(false))), vec![0]);
    assert_eq!(module_ids(Some(&Value::list([Value::Int(7), Value::Bool(true), Value::None]))), vec![7, 1]);
    assert_eq!(module_ids(Some(&Value::list([]))), Vec::<i64>::new());
    assert_eq!(module_ids(Some(&Value::None)), Vec::<i64>::new());
}

#[test]
fn missile_commands_wait_for_boundary_and_use_enqueue_stamp() {
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
    system.insert(ball.clone());
    system.insert(Ball { id: 3, pos: [1000.0, 0.0, 0.0], ..ball.clone() });
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    system.observe(1, 1, tx);
    system.launch_missile(Ball { id: 2, ..ball }, 3, 1, 500, |_| {});
    system.tick(12_999);
    assert_eq!(system.ball(2).unwrap().mode, crate::space::MODE_STOP);
    assert!(rx.try_recv().is_err());
    system.tick(13_000);
    let missile = system.ball(2).unwrap();
    assert_eq!((missile.mode, missile.effect_stamp, missile.follow_id, missile.launcher), (crate::space::MODE_MISSILE, 12, 3, 1));
    let events = rx.try_recv().unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[1], event(12, "LaunchMissile", vec![
        Value::Int(2), Value::Int(3), Value::Int(1), Value::Bool(true), Value::Bool(false),
    ]));
    assert_eq!(system.current_time(), 13);
}


#[test]
fn reset_only_stops_owning_pilots_modules() {
    let mut st = State::default();
    let a = 1;
    let b = 2;
    let flags: Vec<_> = (0..3).map(|_| Arc::new(AtomicBool::new(false))).collect();
    for (master, owner, flag) in [(10, a, &flags[0]), (11, a, &flags[1]), (12, b, &flags[2])] {
        st.active.insert(master, ActiveModule { char_id: owner, stop: flag.clone() });
        st.activation_generation.insert(master, flag.clone());
    }
    st.targets.insert(a, HashSet::from([20]));
    st.targets.insert(b, HashSet::from([a, 21]));
    st.locking.insert((a, 20), 1);
    st.locking.insert((b, a), 2);
    st.locking.insert((b, 21), 3);

    st.reset_character(a);
    assert!(flags[0].load(Ordering::Relaxed));
    assert!(flags[1].load(Ordering::Relaxed));
    assert!(!flags[2].load(Ordering::Relaxed));
    assert!(!st.targets.contains_key(&a));
    assert_eq!(st.targets.get(&b), Some(&HashSet::from([a, 21])));
    assert_eq!(st.locking, HashMap::from([((b, a), 2), ((b, 21), 3)]));
    assert_eq!(st.active.len(), 3);
    assert_eq!(st.activation_generation.len(), 3);
    for (master, flag) in [(10, &flags[0]), (11, &flags[1]), (12, &flags[2])] {
        assert!(Arc::ptr_eq(&st.active[&master].stop, flag));
        assert!(Arc::ptr_eq(&st.activation_generation[&master], flag));
    }
    st.reset_character(3);
    st.reset_character(a);
    assert!(!flags[2].load(Ordering::Relaxed));
    assert_eq!(st.targets.get(&b), Some(&HashSet::from([a, 21])));
    assert_eq!(st.locking, HashMap::from([((b, a), 2), ((b, 21), 3)]));
    assert_eq!(st.active.len(), 3);
    assert_eq!(st.activation_generation.len(), 3);
}


#[test]
fn reload_reservation_survives_departure_and_preserves_new_marker() {
    crate::tests::init_config();
    let mut s = System::default();
    s.insert(Ball {
        id: 42, type_id: 1, owner_id: 1, corp_id: 1, alliance_id: -1,
        char_id: Some(1), signature_radius: Some(1.0), mode: crate::space::MODE_STOP, radius: 1.0, mass: 1.0,
        max_vel: 1.0, agility: 1.0, speed_fraction: 0.0,
        pos: [0.0; 3], vel: [0.0; 3], goto: [0.0; 3],
        old_pos: [0.0; 3], old_vel: [0.0; 3], segment_acceleration: None,
        follow_id: 0, follow_range: 0.0, free: true, global: false, interactive: true,
        massive: true, warp: crate::space::WarpState::default(), warp_factor: 0, last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0,
        cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new(),
    });
    let result = Arc::new(Mutex::new(None));
    let captured = result.clone();
    s.schedule_at(100, move |s| {
        assert!(!s.contains(42));
        let mut markers = HashMap::from([(7, 100)]);
        markers.insert(7, 200);
        clear_reload_markers(&mut markers, &[7], 100);
        *captured.lock().unwrap() = Some(markers);
    });
    s.remove_ball(42).unwrap();
    s.tick(99);
    assert!(result.lock().unwrap().is_none());
    s.tick(100);
    let result = result.lock().unwrap();
    let markers = result.as_ref().unwrap();
    assert_eq!(markers.get(&7), Some(&200));
}

