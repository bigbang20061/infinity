use super::*;

fn ball(id: i64, pilot: Option<i64>, x: f64) -> Ball {
    Ball {
        id, type_id: 1, owner_id: 1, corp_id: 1, alliance_id: -1,
        char_id: pilot, signature_radius: pilot.map(|_| 1.0), mode: MODE_STOP, radius: 1.0, mass: 1e6,
        max_vel: 100.0, agility: 1.0, speed_fraction: 0.0,
        pos: [x, 0.0, 0.0], vel: [0.0; 3], goto: [0.0; 3],
        follow_id: 0, follow_range: 0.0, free: true, global: false,
        massive: true, warp: None, warp_factor: 0, last_ms: 0, effect_stamp: 0,
        cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0,
        slim: Vec::new(),
    }
}

#[test]
fn pilot_index_tracks_insert_take_and_capsule_before_ship_removal() {
    let mut system = System::default();
    system.insert(ball(1, None, 0.0));
    system.insert(ball(2, Some(100), 0.0));
    assert_eq!(system.pilot_ball(100).unwrap().id, 2);
    assert_eq!(system.pilots().count(), 1);
    assert!(system.contains(1));
    assert_eq!(system.balls().count(), 2);

    system.insert(ball(3, Some(100), 0.0));
    assert_eq!(system.take(2).unwrap().char_id, Some(100));
    assert_eq!(system.pilot_ball(100).unwrap().id, 3);
    assert_eq!(system.pilots().count(), 1);
    system.take(3).unwrap();
    assert!(system.pilot_ball(100).is_none());
    assert_eq!(system.pilots().count(), 0);
    assert!(system.take(3).is_none());

    system.insert(ball(4, Some(101), 0.0));
    system.insert(ball(4, Some(102), 0.0));
    assert!(system.pilot_ball(101).is_none());
    assert_eq!(system.pilot_ball(102).unwrap().id, 4);
    system.insert(ball(4, None, 0.0));
    assert!(system.pilot_ball(102).is_none());
    assert!(system.contains(4));
}

#[test]
fn nearest_hostile_via_pilot_index_matches_full_scan() {
    let mut system = System::default();
    system.insert(ball(1, None, 1.0));
    system.insert(ball(2, Some(100), 2.0));
    let mut cloaked = ball(3, Some(101), 3.0);
    cloaked.cloak = 1;
    system.insert(cloaked);
    let mut warping = ball(4, Some(102), 4.0);
    warping.mode = MODE_WARP;
    system.insert(warping);
    system.insert(ball(5, Some(103), 5.0));
    system.insert(ball(6, Some(104), 9.0));
    system.insert(ball(7, Some(105), 20.0));

    fn nearest<'a>(balls: impl Iterator<Item = &'a Ball>, aggro: f64) -> Option<i64> {
        balls
            .filter(|b| b.char_id.is_some_and(|ch| ch != 100)
                && b.cloak == 0 && b.mode != MODE_WARP && len(b.pos) <= aggro)
            .min_by(|a, b| len(a.pos).total_cmp(&len(b.pos)))
            .map(|b| b.id)
    }
    for (range, expected) in [(4.0, None), (5.0, Some(5)), (30.0, Some(5))] {
        let indexed = nearest(system.pilots(), range);
        assert_eq!(indexed, nearest(system.balls(), range));
        assert_eq!(indexed, expected);
    }
    system.take(5);
    assert_eq!(nearest(system.pilots(), 30.0), Some(6));
    assert_eq!(nearest(system.pilots(), 30.0), nearest(system.balls(), 30.0));
}

fn watch(s: &mut System, client: i64, ego: i64) -> mpsc::UnboundedReceiver<Vec<Value>> {
    let (tx, rx) = mpsc::unbounded_channel();
    s.observe(client, ego, tx);
    rx
}

fn name(event: &Value) -> &[u8] {
    let Value::Tuple(parts) = event else { panic!("event tuple") };
    let Value::Tuple(call) = &parts[1] else { panic!("call tuple") };
    let Value::Bytes(name) = &call[0] else { panic!("event name") };
    name
}

#[test]
fn command_stamps_seconds_and_borrows_other_balls() {
    let mut s = System::default();
    s.insert(ball(1, Some(100), 0.0));
    s.insert(ball(2, None, 10.0));
    let mut rx = watch(&mut s, 1, 1);
    s.evolve_all(12_345);
    s.command(1, |b, stamp, others| {
        assert!(others.ball(1).is_none());
        b.goto = others.ball(2).unwrap().pos;
        vec![event(stamp, "GotoDirection", vec![Value::Int(b.id)])]
    });
    let events = rx.try_recv().unwrap();
    let Value::Tuple(parts) = &events[0] else { panic!("event") };
    assert_eq!(parts[0].as_int(), Some(12));
    assert_eq!(s.ball(1).unwrap().goto, [10.0, 0.0, 0.0]);
}

#[test]
fn cloak_visibility_and_tick_lapse_are_batched_per_observer() {
    let mut s = System::default();
    let mut b = ball(1, Some(100), 0.0);
    b.cloak = CLOAK_RESTORE;
    b.cloak_until = crate::client::filetime_from_unix_ms(1000);
    s.insert(b);
    let mut own = watch(&mut s, 1, 1);
    let mut other = watch(&mut s, 2, 2);
    s.broadcast(1, vec![event(0, "private", vec![])]);
    assert_eq!(name(&own.try_recv().unwrap()[0]), b"private");
    assert!(other.try_recv().is_err());
    s.tick(999);
    assert!(own.try_recv().is_err());
    s.tick(1000);
    assert_eq!(s.ball(1).unwrap().cloak, 0);
    assert_eq!(name(&own.try_recv().unwrap()[0]), b"UncloakBall");
    assert_eq!(name(&other.try_recv().unwrap()[0]), b"AddBalls");
    assert!(other.try_recv().is_err());
}

#[test]
fn pilot_removal_issues_jump_queue_key() {
    let registry = Arc::new(Pilots { capacity: 1, ..Pilots::default() });
    let (tx, mut rx) = mpsc::unbounded_channel();
    registry.register_pilot(200, tx);
    let mut s = System::new(42, registry);
    s.insert(ball(1, Some(100), 0.0));
    assert_eq!(s.request_entry(200), Err(1));
    rx.try_recv().unwrap();
    s.remove_ball(1).unwrap();
    let (name, args) = rx.try_recv().unwrap();
    assert_eq!(name, "OnJumpQueueUpdate");
    let Value::Dict(keys) = &args[3] else { panic!("jump keys") };
    assert_eq!(keys[0].0.as_int(), Some(200));
    assert_eq!(s.request_entry(200), Ok(()));
}

#[test]
fn closed_observers_are_pruned_even_when_cold() {
    let mut s = System::default();
    drop(watch(&mut s, 1, 1));
    s.tick(100);
    assert!(s.observers.is_empty());
    drop(watch(&mut s, 2, 2));
    s.broadcast(0, vec![event(0, "test", vec![])]);
    assert!(s.observers.is_empty());
}

#[test]
fn warp_arrives_at_millisecond_deadline() {
    let mut s = System::default();
    let mut b = ball(1, None, 0.0);
    b.mode = MODE_WARP;
    b.warp = Some(Warp { from: b.pos, to: [100.0, 0.0, 0.0], start: 100,
        duration: 1000.0, factor: 3000 });
    b.effect_stamp = destiny_stamp(100);
    s.insert(b);
    s.tick(600);
    assert_eq!(s.ball(1).unwrap().pos, [50.0, 0.0, 0.0]);
    s.tick(1100);
    let b = s.ball(1).unwrap();
    assert_eq!(b.pos, [100.0, 0.0, 0.0]);
    assert_eq!(b.mode, MODE_STOP);
    assert!(b.warp.is_none());
    assert_eq!(b.vel, [0.0; 3]);
}

#[test]
fn set_state_contains_visible_balls_ego_damage_and_system() {
    let mut s = System::new(42, Arc::default());
    s.insert(ball(1, Some(100), 0.0));
    let mut hidden = ball(2, Some(200), 1.0);
    hidden.cloak = CLOAK_RESTORE;
    s.insert(hidden);
    let (tx, _rx) = mpsc::unbounded_channel();
    let state = s.observe(1, 1, tx);
    assert_eq!(name(&state), b"SetState");
    let Value::Tuple(parts) = state else { panic!("event") };
    let Value::Tuple(call) = &parts[1] else { panic!("call") };
    let Value::Tuple(args) = &call[1] else { panic!("args") };
    let Value::Instance(_, bag) = &args[0] else { panic!("bag") };
    let Value::Dict(fields) = &**bag else { panic!("fields") };
    let field = |key: &str| &fields.iter().find(|(k, _)| k == &Value::str(key)).unwrap().1;
    assert_eq!(field("ego").as_int(), Some(1));
    let Value::List(slims) = field("slims") else { panic!("slims") };
    assert_eq!(slims.len(), 1);
    let Value::Dict(damage) = field("damageState") else { panic!("damage") };
    assert_eq!(damage.len(), 1);
    assert_eq!(damage[0].0.as_int(), Some(1));
    let Value::Bytes(bytes) = field("state") else { panic!("state") };
    assert_eq!(bytes[0], DESTINY_FULLSTATE);
    assert_eq!(i64::from_le_bytes(bytes[5..13].try_into().unwrap()), 1);
    let Value::Instance(_, sol) = field("solItem") else { panic!("system") };
    let Value::Dict(sol) = &**sol else { panic!("system fields") };
    assert!(sol.iter().any(|(k,v)| k == &Value::str("itemID") && v.as_int() == Some(42)));
}

#[test]
fn constant_target_integration_is_step_size_independent() {
    let mut one = ball(1, None, 7.0);
    one.mode = MODE_GOTO;
    one.goto = [1.0, 2.0, 3.0];
    one.speed_fraction = 0.75;
    one.vel = [-3.0, 4.0, 2.0];
    let mut ten = one.clone();
    let positions = HashMap::new();
    one.evolve(1000, &positions);
    for ms in (100..=1000).step_by(100) { ten.evolve(ms, &positions); }
    for (a, b) in one.pos.into_iter().zip(ten.pos).chain(one.vel.into_iter().zip(ten.vel)) {
        assert!((a - b).abs() <= 1e-6 * a.abs().max(1.0));
    }
}

#[test]
fn timers_fire_by_deadline_not_insertion_order_without_firing_early() {
    let mut s = System::default();
    let fired = Arc::new(Mutex::new(Vec::new()));
    s.evolve_all(1000);
    for (delay, label) in [(250, 3), (101, 2), (50, 1)] {
        let fired = fired.clone();
        s.schedule(delay, move |s| fired.lock().unwrap().push((label, s.ms)));
    }
    s.tick(1000);
    assert!(fired.lock().unwrap().is_empty());
    s.tick(1100);
    assert_eq!(*fired.lock().unwrap(), [(1, 1100)]);
    s.tick(1300);
    assert_eq!(*fired.lock().unwrap(), [(1, 1100), (2, 1300), (3, 1300)]);
    s.tick(1400);
    assert_eq!(fired.lock().unwrap().len(), 3);
}

#[test]
fn same_due_timers_fire_in_scheduling_order_including_nested_schedule() {
    let mut s = System::default();
    let fired = Arc::new(Mutex::new(Vec::new()));
    let first = fired.clone();
    s.schedule(100, move |s| {
        first.lock().unwrap().push(1);
        s.schedule(0, move |_| first.lock().unwrap().push(3));
    });
    let second = fired.clone();
    s.schedule(100, move |_| second.lock().unwrap().push(2));
    s.tick(100);
    assert_eq!(*fired.lock().unwrap(), [1, 2, 3]);
}

#[test]
fn timer_for_removed_ball_is_harmless_and_other_timers_still_fire() {
    let mut s = System::default();
    s.insert(ball(1, None, 0.0));
    s.schedule(100, |s| {
        assert!(s.destroy_ball(1).is_none());
        assert!(!s.contains(1));
    });
    s.schedule(100, |s| s.insert(ball(2, None, 0.0)));
    s.remove_ball(1).unwrap();
    s.tick(100);
    assert!(s.contains(2));
    assert!(s.timers.is_empty());
}

#[test]
fn future_timer_allows_cold_skip_until_due() {
    let mut s = System::default();
    s.schedule(1000, |s| assert_eq!(s.ms, 1000));
    assert!(s.is_cold());
    s.tick(100);
    assert_eq!(s.ms, 100);
    assert!(s.is_cold());
    s.tick(1000);
    assert!(s.timers.is_empty());
    assert!(s.is_cold());
}

#[test]
fn absolute_deadlines_preserve_cadence_and_tie_order() {
    let mut s = System::default();
    let result = Arc::new(Mutex::new(Vec::new()));
    let first = result.clone();
    s.schedule_at(150, move |s| {
        first.lock().unwrap().push((1, s.now_ms()));
        s.schedule_at(300, move |s| first.lock().unwrap().push((3, s.now_ms())));
    });
    let second = result.clone();
    s.schedule_at(150, move |s| second.lock().unwrap().push((2, s.now_ms())));
    s.tick(200);
    s.tick(300);
    assert_eq!(*result.lock().unwrap(), [(1, 200), (2, 200), (3, 300)]);
}
