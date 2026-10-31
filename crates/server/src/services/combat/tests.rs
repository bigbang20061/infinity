use super::*;

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
fn propulsion_and_missile_commands_wait_for_boundary_and_use_enqueue_stamp() {
    let mut system = System::new(0, Arc::default(), 12_345);
    let ball = Ball {
        id: 1, type_id: 1, owner_id: 1, corp_id: 1, alliance_id: -1,
        char_id: None, signature_radius: None, mode: crate::space::MODE_STOP,
        radius: 1.0, mass: 1.0, max_vel: 1.0, agility: 1.0, speed_fraction: 0.0,
        pos: [0.0; 3], vel: [0.0; 3], goto: [0.0; 3], follow_id: 0,
        old_pos: [0.0; 3], old_vel: [0.0; 3], segment_acceleration: None, segment_warp: None,
        follow_range: 0.0, free: true, global: false, massive: false, interactive: false,
        warp: None, warp_factor: 0, last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0, cloak: 0,
        cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new(),
    };
    system.insert(ball.clone());
    system.insert(Ball { id: 3, pos: [1000.0, 0.0, 0.0], ..ball.clone() });
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    system.observe(1, 1, tx);
    queue_kinematics(&mut system, 1, Kinematics { speed: 20.0, mass: 30.0, agility: 0.25, signature: 40.0 });
    system.launch_missile(Ball { id: 2, ..ball }, 3, 1, 500, |_| {});
    system.tick(12_999);
    assert_eq!(system.ball(1).unwrap().max_vel, 1.0);
    assert_eq!(system.ball(1).unwrap().agility, 1.0);
    assert_eq!(system.ball(2).unwrap().mode, crate::space::MODE_STOP);
    assert!(rx.try_recv().is_err());
    system.tick(13_000);
    let ship = system.ball(1).unwrap();
    assert_eq!((ship.max_vel, ship.mass, ship.agility, ship.signature_radius), (20.0, 30.0, 0.25, Some(40.0)));
    let missile = system.ball(2).unwrap();
    assert_eq!((missile.mode, missile.effect_stamp, missile.follow_id, missile.launcher), (crate::space::MODE_MISSILE, 12, 3, 1));
    let events = rx.try_recv().unwrap();
    assert_eq!(events.len(), 5);
    assert_eq!(events[0], event(12, "SetMaxSpeed", vec![Value::Int(1), Value::Float(20.0)]));
    assert_eq!(events[1], event(12, "SetBallMass", vec![Value::Int(1), Value::Float(30.0)]));
    assert_eq!(events[2], event(12, "SetBallAgility", vec![Value::Int(1), Value::Float(0.25)]));
    assert_eq!(events[4], event(12, "LaunchMissile", vec![
        Value::Int(2), Value::Int(3), Value::Int(1), Value::Bool(true), Value::Bool(false),
    ]));
    assert_eq!(system.current_time(), 13);
}

#[test]
fn missile_lifecycle_preserves_exploration_site() {
    let mut system = System::new(0, Arc::default(), 12_345);
    let site = Ball {
        id: 9_500_000_000_000, type_id: 1, owner_id: 1, corp_id: 1, alliance_id: -1,
        char_id: None, signature_radius: None, mode: crate::space::MODE_STOP, radius: 1.0, mass: 1.0,
        max_vel: 1.0, agility: 1.0, speed_fraction: 0.0,
        pos: [1.0, 2.0, 3.0], vel: [0.0; 3], goto: [0.0; 3],
        old_pos: [1.0, 2.0, 3.0], old_vel: [0.0; 3], segment_acceleration: None, segment_warp: None,
        follow_id: 0, follow_range: 0.0, free: true, global: false, interactive: false,
        massive: true, warp: None, warp_factor: 0, last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0,
        cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new(),
    };
    let (site_id, site_type, site_pos) = (site.id, site.type_id, site.pos);
    let missile_id = NEXT_MISSILE_ID.fetch_add(1, Ordering::Relaxed);
    let missile = Ball {
        id: missile_id, type_id: 2, mode: crate::space::MODE_STOP, pos: [4.0, 5.0, 6.0],
        ..site.clone()
    };
    system.insert(site);
    system.launch_missile(missile, site_id, site_id, 500, move |s| { s.destroy_ball(missile_id); });

    let retained_site = system.ball(site_id).unwrap();
    assert_eq!((retained_site.id, retained_site.type_id, retained_site.pos), (site_id, site_type, site_pos));
    let retained_missile = system.ball(missile_id).unwrap();
    assert_eq!((retained_missile.id, retained_missile.type_id, retained_missile.pos), (missile_id, 2, [4.0, 5.0, 6.0]));
    let second_id = NEXT_MISSILE_ID.fetch_add(1, Ordering::Relaxed);
    assert_ne!(missile_id, second_id);
    assert_eq!(second_id, missile_id + 1);

    system.tick(13_000);
    system.tick(13_500);
    assert!(!system.contains(missile_id));
    let retained_site = system.ball(site_id).unwrap();
    assert_eq!((retained_site.id, retained_site.type_id, retained_site.pos), (site_id, site_type, site_pos));
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
fn discarded_cycle_owners_release_only_their_generation() {
    let master = -9011;
    let generation = Arc::new(AtomicBool::new(false));
    {
        let mut st = state();
        assert!(!st.active.contains_key(&master));
        st.active.insert(master, ActiveModule { char_id: 1, stop: generation.clone() });
        st.activation_generation.insert(master, generation.clone());
    }
    let owner = ActivationOwner { master, generation: generation.clone(), armed: true };
    let mut system = System::default();
    schedule_cycle_end(&mut system, 0, 1000.0, move |_, _| drop(owner));
    drop(system);
    {
        let mut st = state();
        assert!(!st.active.contains_key(&master));
        assert!(!st.activation_generation.contains_key(&master));
        st.active.insert(master, ActiveModule { char_id: 1, stop: generation.clone() });
        st.activation_generation.insert(master, generation.clone());
    }
    let owner = ActivationOwner { master, generation: generation.clone(), armed: true };
    let system = System::default();
    system.defer(async move { drop(owner); });
    {
        let st = state();
        assert!(!st.active.contains_key(&master));
        assert!(!st.activation_generation.contains_key(&master));
    }
    {
        let mut st = state();
        st.active.insert(master, ActiveModule { char_id: 1, stop: generation.clone() });
        st.activation_generation.insert(master, generation.clone());
    }
    let owner = ActivationOwner { master, generation: generation.clone(), armed: true };
    let rejected_post = move |_: &mut System| drop(owner);
    drop(rejected_post);
    {
        let st = state();
        assert!(!st.active.contains_key(&master));
        assert!(!st.activation_generation.contains_key(&master));
    }

    let replacement = Arc::new(AtomicBool::new(false));
    {
        let mut st = state();
        st.active.insert(master, ActiveModule { char_id: 1, stop: replacement.clone() });
        st.activation_generation.insert(master, replacement.clone());
    }
    let owner = ActivationOwner { master, generation, armed: true };
    let mut system = System::default();
    schedule_cycle_end(&mut system, 0, 1000.0, move |_, _| drop(owner));
    drop(system);
    let mut st = state();
    assert!(Arc::ptr_eq(&st.active[&master].stop, &replacement));
    assert!(Arc::ptr_eq(&st.activation_generation[&master], &replacement));
    st.active.remove(&master);
    st.activation_generation.remove(&master);
}


#[test]
fn cycle_deadlines_and_deactivation() {
    let mut s = System::default();
    let completed = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let seen = completed.clone();
    let flag = stop.clone();
    schedule_cycle_end(&mut s, 1000, 500.0, move |s, end| {
        seen.lock().unwrap().push((1, s.now_ms(), end));
        assert!(!cycle_is_final(flag.load(Ordering::Relaxed), 3, 1));
        // Model a continuation returning late from the worker.
        s.schedule_at(1700, move |s| {
            assert!(!cycle_should_stop(false, &flag));
            schedule_cycle_end(s, end, 500.0, move |s, end| {
                seen.lock().unwrap().push((2, s.now_ms(), end));
                assert!(cycle_is_final(flag.load(Ordering::Relaxed), 3, 2));
                assert!(cycle_should_stop(false, &flag));
            });
        });
    });
    s.tick(1499);
    assert!(completed.lock().unwrap().is_empty());
    s.tick(1500);
    s.tick(1700);
    stop.store(true, Ordering::Relaxed);
    s.tick(1999);
    assert_eq!(*completed.lock().unwrap(), [(1, 1500, 1500)]);
    s.tick(2000);
    s.tick(3000);
    assert_eq!(*completed.lock().unwrap(), [(1, 1500, 1500), (2, 2000, 2000)]);

    let old = Arc::new(AtomicBool::new(false));
    let replacement = Arc::new(AtomicBool::new(false));
    let mut active = HashMap::from([(1, ActiveModule { char_id: 1, stop: replacement.clone() })]);
    let mut published = false;
    assert!(!retire_activation(&mut active, 1, &old, || published = true));
    assert!(!published);
    assert!(Arc::ptr_eq(&active[&1].stop, &replacement));
    assert!(retire_activation(&mut active, 1, &replacement, || published = true));
    assert!(published);
    assert!(active.is_empty());
}

#[test]
fn reload_reservation_survives_departure_and_preserves_new_marker() {
    let mut s = System::default();
    s.insert(Ball {
        id: 42, type_id: 1, owner_id: 1, corp_id: 1, alliance_id: -1,
        char_id: Some(1), signature_radius: Some(1.0), mode: crate::space::MODE_STOP, radius: 1.0, mass: 1.0,
        max_vel: 1.0, agility: 1.0, speed_fraction: 0.0,
        pos: [0.0; 3], vel: [0.0; 3], goto: [0.0; 3],
        old_pos: [0.0; 3], old_vel: [0.0; 3], segment_acceleration: None, segment_warp: None,
        follow_id: 0, follow_range: 0.0, free: true, global: false, interactive: true,
        massive: true, warp: None, warp_factor: 0, last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0,
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

#[test]
fn hit_quality_bands() {
    assert_eq!(hit_quality(0.0), 0);
    assert_eq!(hit_quality(0.5), 1);
    assert_eq!(hit_quality(1.1), 4);
    assert_eq!(hit_quality(3.0), 6);
}

#[test]
fn weapon_effects() {
    assert!(weapon_kind("projectileFired") == Some(Weapon::Turret));
    assert!(weapon_kind("useMissiles") == Some(Weapon::Missile));
    assert!(weapon_kind("online").is_none());
}
