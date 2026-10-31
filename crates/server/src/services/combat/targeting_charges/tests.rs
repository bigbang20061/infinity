use super::*;
use crate::services::effects::{projection::Projection, EffectKind, JammingType};
use crate::space::System;

mod auto_reload_db;


#[test]
fn pending_locks_recheck_generation_validity_and_capacity() {
    let mut st = State::default();
    st.locking.insert((1, 10), 2);
    assert_eq!(finish_lock(&mut st, 100, 1, 10, 1, 0, true, 1, None), (false, false));
    assert_eq!(finish_lock(&mut st, 100, 1, 10, 2, 0, false, 1, None), (true, false));
    assert!(st.targets.get(&1).is_none());
    st.locking.insert((1, 10), 3);
    assert_eq!(finish_lock(&mut st, 100, 1, 10, 3, 0, true, 1, None), (true, true));
    st.locking.insert((1, 11), 4);
    assert_eq!(finish_lock(&mut st, 100, 1, 11, 4, 0, true, 1, None), (true, false));
    assert_eq!(st.targets[&1], HashSet::from([10]));
}

#[test]
fn cycle_lock_completion_survives_retirement_and_replacement_but_not_stop() {
    for case in 0..5 {
        let mut st = State::default();
        let generation = Arc::new(AtomicBool::new(false));
        let owner = (2, generation.clone());
        st.active.insert(2, ActiveModule { char_id: 1, stop: generation.clone(), forced_inactive: false });
        assert!(lock_owner_valid(&st, Some(&owner)));
        st.locking.insert((1, 10), 1);
        match case {
            0 => generation.store(true, Ordering::Relaxed),
            1 => st.active.get_mut(&2).unwrap().stop = Arc::new(AtomicBool::new(false)),
            2 => {
                st.active.get_mut(&2).unwrap().forced_inactive = true;
                generation.store(true, Ordering::Relaxed);
            }
            3 => { st.active.remove(&2); },
            _ => {}
        }
        let completes = case != 0 && case != 2;
        assert_eq!(lock_owner_valid(&st, Some(&owner)), case == 4);
        assert_eq!(finish_lock(&mut st, 100, 1, 10, 1, 0, true, 1, Some(&owner)), (true, completes));
        assert!(!st.locking.contains_key(&(1, 10)));
        assert_eq!(st.targets.get(&1).is_some_and(|targets| targets.contains(&10)), completes);
    }
}

#[test]
fn jam_rejects_cycle_lock_completion_after_owner_retirement() {
    crate::tests::init_config();
    let ship = NEXT_LOCK.fetch_add(4, Ordering::Relaxed) as i64;
    let pilot = ship + 1;
    let target = ship + 2;
    let module = ship + 3;
    let mut system = System::new(ship, Arc::default(), 1000);
    system.insert(Ball {
        id: ship, type_id: 1, owner_id: pilot, corp_id: 1, alliance_id: 1,
        char_id: Some(pilot), signature_radius: None, mode: crate::space::MODE_STOP,
        radius: 1.0, mass: 1.0, max_vel: 0.0, agility: 1.0, speed_fraction: 0.0,
        pos: [0.0; 3], vel: [0.0; 3], old_pos: [0.0; 3], old_vel: [0.0; 3],
        segment_acceleration: None, goto: [0.0; 3], follow_id: 0, follow_range: 0.0,
        free: false, global: false, massive: true, interactive: true,
        warp: crate::space::WarpState::default(), warp_factor: 0, last_ms: 0,
        interpolation_anchor_ms: 0, effect_stamp: 0, cloak: 0, cloak_until: 0,
        health: [1.0; 3], launcher: 0, slim: Vec::new(),
    });
    let generation = Arc::new(AtomicBool::new(false));
    let owner = (module, generation);
    jamming::apply(&mut system, target, module, ship, 100.0);
    let mut st = state();
    st.locking.insert((pilot, target), 1);
    assert_eq!(finish_lock(&mut st, ship, pilot, target, 1, 1000, true, 1, Some(&owner)), (true, false));
    assert!(!st.targets.contains_key(&pilot));
    drop(st);
    jamming::clear(&mut system, ship);
}

#[test]
fn auto_target_candidates_only_include_pilots_locking_the_ship() {
    let mut st = State::default();
    st.targets.insert(1, HashSet::from([10]));
    st.targets.insert(2, HashSet::from([11]));
    st.locking.insert((3, 10), 1);
    assert_eq!(targeters_in(&st, 10), vec![1]);
    assert_eq!(targeters_in(&st, 11), vec![2]);
    assert!(targeters_in(&st, 12).is_empty());
}

#[test]
fn passive_lock_notifies_owner_without_yellow_box() {
    for silent in [false, true] {
        let mut owner = Vec::new();
        let mut other = Vec::new();
        notify_lock(20, 10, silent, |args| owner = args, |args| other = args);
        assert_eq!(owner[0].as_bytes(), Some(b"add".as_slice()));
        assert_eq!(owner[1].as_int(), Some(20));
        if silent { assert!(other.is_empty()); } else {
            assert_eq!(other[0].as_bytes(), Some(b"otheradd".as_slice()));
            assert_eq!(other[1].as_int(), Some(10));
        }
    }
}

#[test]
fn loss_notices_only_consume_announced_locks() {
    let mut st = State::default();
    st.targets.insert(1, HashSet::from([20, 30]));
    st.announced_locks.insert((1, 20), (10, 2));
    assert!(take_announced(&mut st, |ch, target| ch == 1 && target == 30).is_empty());
    assert_eq!(take_announced(&mut st, |ch, target| ch == 1 && target == 20), vec![(10, 2)]);
    assert!(take_announced(&mut st, |_, _| true).is_empty());
}

#[test]
fn cloak_lock_invalidation_preserves_unrelated_locks_and_capacitor() {
    let mut st = State::default();
    st.targets.insert(1, HashSet::from([20, 30]));
    st.targets.insert(2, HashSet::from([10, 40]));
    st.locking.insert((1, 50), 1);
    st.locking.insert((2, 10), 2);
    st.locking.insert((2, 40), 3);
    st.capacitor.insert(10, (42.0, 1));
    let (incoming, outgoing) = clear_lock_state(&mut st, 10, 1);
    assert_eq!(incoming, vec![2]);
    assert_eq!(outgoing, HashSet::from([20, 30]));
    assert_eq!(st.targets[&2], HashSet::from([40]));
    assert_eq!(st.locking.len(), 1);
    assert_eq!(st.capacitor[&10], (42.0, 1));
}

#[test]
fn reload_groups_use_each_modules_deadline_and_charge_type() {
    let loads = vec![
        PendingLoad { reload_id: 1, load: (1, 1, 10, 5, 0) },
        PendingLoad { reload_id: 2, load: (2, 2, 10, 3, 0) },
        PendingLoad { reload_id: 3, load: (3, 3, 10, 1, 0) },
        PendingLoad { reload_id: 4, load: (4, 4, 11, 1, 0) },
    ];
    let groups = group_loads(loads, |id| if id == 2 { 2000.0 } else { 1000.0 });
    let ids = |key| groups[&key].iter().map(|(load, _)| load.load.0).collect::<Vec<_>>();
    assert_eq!(ids((filetime_ticks_from_ms(1000.0), 10)), vec![1, 3]);
    assert_eq!(ids((filetime_ticks_from_ms(2000.0), 10)), vec![2]);
    assert_eq!(ids((filetime_ticks_from_ms(1000.0), 11)), vec![4]);
}

#[test]
fn exhausted_stock_only_counts_modules_with_reservations() {
    let item = Item { item_id: 1, type_id: 10, owner_id: 1, location_id: 2, flag_id: FLAG_CARGO,
        quantity: 6, singleton: false, group_id: 0, category_id: 0, custom_info: None };
    let mut items = BTreeMap::from([(1, (item, 6))]);
    let reservations: Vec<_> = (0..3).filter_map(|_| reserve_load(&mut items, &[1], 10, 5, 0)).collect();
    assert_eq!(reservations, vec![5, 1]);
    assert_eq!(reservations.len(), 2);
    assert_eq!(items[&1].1, 0);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with a grouped weapon"]
async fn member_resolution_is_separate_from_master_only_bank() {
    let mut config = crate::config::tests::test_database();
    config.schema_on_start = false;
    let db = Db::connect(&config).await.expect("database pool");
    let ship: i64 = db.query_one("SELECT ship_id FROM game.weapon_banks ORDER BY ship_id LIMIT 1", &[]).await.expect("grouped weapon fixture").get("ship_id");
    let rows = crate::dao::items::charges::banks(&db, ship).await.expect("banks");
    let row = rows.first().expect("grouped weapon fixture");
    let resolved = master_for_member(&db, ship, row.slave_id).await.unwrap_or_else(|_| panic!("member master"));
    assert_eq!(resolved, row.master_id);
    assert_eq!(master_for_member(&db, ship, row.master_id).await.unwrap_or_else(|_| panic!("master")), row.master_id);
    let master = bank(&db, ship, resolved).await.unwrap_or_else(|_| panic!("master bank"));
    assert_eq!(master[0], row.master_id);
    for member in rows.iter().filter(|r| r.master_id == row.master_id) {
        assert!(master.contains(&member.slave_id));
    }
    let mut slave = bank(&db, ship, row.slave_id).await.unwrap_or_else(|_| panic!("slave bank"));
    assert_eq!(slave[0], row.slave_id);
    let mut expected: Vec<_> = std::iter::once(row.slave_id)
        .chain(rows.iter().filter(|r| r.master_id == row.slave_id).map(|r| r.slave_id)).collect();
    slave.sort_unstable();
    expected.sort_unstable();
    assert_eq!(slave, expected);
}

#[test]
fn removed_locks_clear_only_matching_outgoing_projections() {
    let mut s = System::new(0, Arc::default(), 1_000);
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
    for id in 1..=4 { s.insert(Ball { id, ..ball.clone() }); }
    let projection = |source_ship, source_module| Projection {
        source_ship, source_module, source_type: 1, effect_id: 1,
        kind: EffectKind::Projected { jamming: JammingType::Webify }, until_ms: i64::MAX,
        values: vec![], charge_values: vec![], charge_type: None,
    };
    s.upsert_projection(2, projection(1, 10));
    s.upsert_projection(3, projection(1, 11));
    s.upsert_projection(2, projection(4, 12));
    let revision = s.projection_revision(2);
    remove_target_projections_in_system(&mut s, 1, &[2]);
    assert!(s.projection_revision(2) > revision);
    assert_eq!(s.projections_on(2).iter().map(|p| p.source_module).collect::<Vec<_>>(), vec![12]);
    assert_eq!(s.projections_on(3).len(), 1);
    remove_target_projections_in_system(&mut s, 1, &[2, 3]);
    assert_eq!(s.projections_on(2).len(), 1);
    assert!(s.projections_on(3).is_empty());
}

#[test]
fn compatible_groups_and_sizes_are_required() {
    assert!(charge_matches([10.0, 20.0].into_iter(), Some(20), Some(2.0), Some(2.0)));
    assert!(!charge_matches([10.0].into_iter(), Some(20), Some(2.0), Some(2.0)));
    assert!(!charge_matches([20.0].into_iter(), Some(20), Some(2.0), Some(1.0)));
    assert!(!charge_matches([20.0].into_iter(), Some(20), Some(2.0), None));
    assert!(!charge_matches([].into_iter(), None, None, None));
    assert!(charge_matches([20.0].into_iter(), Some(20), None, None));
}
