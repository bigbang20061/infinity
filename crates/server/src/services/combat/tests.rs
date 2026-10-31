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
        st.active.insert(master, ActiveModule { char_id: owner, stop: flag.clone(), forced_inactive: false });
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

#[test]
fn restored_condition_recharges_only_shield_and_preserves_damage() {
    let stats = ShipStats { max: [100.0, 200.0, 300.0], resonance: [[1.0; 4]; 3], shield_tau_ms: 1000.0 };
    let values = [Some(25.0), Some(40.0), Some(90.0)];
    let (initial, fractions) = restored_condition(&stats, values, 0, FILETIME_SECOND);
    assert_eq!(initial, [25.0, 160.0, 210.0]);
    assert_eq!(fractions, [0.25, 0.8, 0.7]);
    let (later, _) = restored_condition(&stats, values, FILETIME_SECOND, 2 * FILETIME_SECOND);
    assert!(later[0] > initial[0] && later[0] < stats.max[0]);
    assert_eq!(&later[1..], &initial[1..]);
    let (future, _) = restored_condition(&stats, values, 2 * FILETIME_SECOND, FILETIME_SECOND);
    assert_eq!(future, initial);
}


#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn condition_persists_recharges_and_rollback_preserves_all_layers() {
    use crate::{config::deployment::DatabaseConfig, consts::CATEGORY_SHIP, dao::items::dogma};
    crate::tests::init_config();
    let db = Db::connect(&DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let ty = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)
        && [ATTR_SHIELD_CAPACITY, ATTR_ARMOR_HP, ATTR_STRUCTURE_HP, ATTR_SHIELD_RECHARGE_RATE].iter().all(|&a| t.attribute(a).unwrap_or(0.0) > 10.0)).expect("ship with condition attributes").type_id;
    let station = sde.map.stations().next().unwrap().station_id;
    let account: i64 = db.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES (nextval('game.accounts_user_id_seq')::text, ''::bytea, 0) RETURNING user_id", &[]).await.unwrap().get("user_id");
    let owner: i64 = db.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id, station_id, balance) VALUES ($1, nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1, $2, 0) RETURNING character_id", &[&account, &station]).await.unwrap().get("character_id");
    let ship = crate::dao::items::creation::rookie_ship(&db, ty, owner, station, FLAG_HANGAR).await.unwrap();
    let world = World::new(db.clone(), sde, 1);
    let fit = Fit::load(&db, world.sde().clone(), owner, ship).await.ok().unwrap();
    let stats = ShipStats::from_fit(&fit);
    let levels = [stats.max[0] * 0.25, stats.max[1] - 3.0, stats.max[2] - 2.0];
    state().condition.insert(ship, Condition { stats: stats.clone(), now: levels, shield_at: filetime_now() });
    assert_eq!(damage_player(&db, &world, -1, ship, owner, [100.0; 4], None).await.ok(), Some(0.0));
    assert!(repair_player(&db, &world, -1, ship, owner, 1, 1.0).await.is_ok());
    assert_eq!(state().condition[&ship].now, levels);
    assert!(save_condition(&db, ship).await.is_ok());
    let rows = dogma::overrides(&db, ship).await.unwrap();
    let ids = [ATTR_SHIELD_CHARGE, ATTR_ARMOR_DAMAGE, ATTR_HULL_DAMAGE];
    let original = ids.map(|id| {
        let row = rows.iter().find(|r| r.attribute_id == id).unwrap();
        (row.value, row.updated_at)
    });
    assert_eq!(original[1].0, 3.0);
    assert_eq!(original[2].0, 2.0);
    assert!(original[0].1 > 0);
    let result: Result<(), CallError> = db.transaction(|tx, _: &mut Outbox| Box::pin(async move {
        dogma::save_condition(tx, ship, &ids, &[1.0, 1.0, 1.0], ATTR_SHIELD_CHARGE, 1).await.map_err(db_err)?;
        Err(CallError::Internal(anyhow::anyhow!("rollback")))
    }), |(), _| async { Ok(()) }).await;
    assert!(result.is_err());
    let rows = dogma::overrides(&db, ship).await.unwrap();
    assert_eq!(ids.map(|id| {
        let row = rows.iter().find(|r| r.attribute_id == id).unwrap();
        (row.value, row.updated_at)
    }), original);
    let elapsed_at = filetime_now() - FILETIME_SECOND * 60;
    dogma::save_condition(&db, ship, &ids, &[levels[0], 3.0, 2.0], ATTR_SHIELD_CHARGE, elapsed_at).await.unwrap();
    invalidate_condition(ship);
    let health = load_docked_condition(&db, &world, owner, ship).await.ok().unwrap();
    assert!(health[0] > levels[0] / stats.max[0]);
    assert!((health[1] - levels[1] / stats.max[1]).abs() < 1e-9);
    assert!((health[2] - levels[2] / stats.max[2]).abs() < 1e-9);
    invalidate_condition(ship);
    db.execute("DELETE FROM game.item_attributes WHERE item_id = $1", &[&ship]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id = $1", &[&ship]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE character_id = $1", &[&owner]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&account]).await.unwrap();
}

async fn load_space_condition(db: &Db, world: &World, system: i64, owner: i64, ship: i64) -> [f64; 3] {
    let fit = Fit::load_in_space(db, world, system, owner, ship).await.ok().unwrap();
    load_condition_with_fit(db, ship, &fit).await.ok().unwrap()
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn gm_damage_survives_reload_and_snapshot_then_heal_clears_it() {
    use crate::{config::{Config, deployment::DatabaseConfig}, conn::Conn, consts::CATEGORY_SHIP, dao::items::dogma, handshake::Login};
    crate::tests::init_config();
    let db = Db::connect(&DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let ty = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)
        && [ATTR_SHIELD_CAPACITY, ATTR_ARMOR_HP, ATTR_STRUCTURE_HP, ATTR_SHIELD_RECHARGE_RATE].iter().all(|&a| t.attribute(a).unwrap_or(0.0) > 10.0)).expect("ship with condition attributes").type_id;
    let station = sde.map.stations().next().unwrap().station_id;
    let account: i64 = db.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES (nextval('game.accounts_user_id_seq')::text, ''::bytea, 0) RETURNING user_id", &[]).await.unwrap().get("user_id");
    let owner: i64 = db.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id, station_id, balance) VALUES ($1, nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1, $2, 0) RETURNING character_id", &[&account, &station]).await.unwrap().get("character_id");
    let ship = crate::dao::items::creation::rookie_ship(&db, ty, owner, station, FLAG_HANGAR).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let system = -1;
    world.add_ship(system, Ball {
        id: ship, type_id: ty, owner_id: owner, corp_id: 1, alliance_id: -1,
        char_id: Some(owner), signature_radius: Some(1.0), mode: crate::space::MODE_STOP, radius: 1.0, mass: 1.0,
        max_vel: 1.0, agility: 1.0, speed_fraction: 0.0,
        pos: [0.0; 3], vel: [0.0; 3], goto: [0.0; 3],
        old_pos: [0.0; 3], old_vel: [0.0; 3], segment_acceleration: None,
        follow_id: 0, follow_range: 0.0, free: true, global: false, interactive: true,
        massive: true, warp: crate::space::WarpState::default(), warp_factor: 0, last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0,
        cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new(),
    }).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let (stream, addr) = listener.accept().await.unwrap();
    let login = Login { user_id: account, user_name: account.to_string(), role: 0, client_id: owner, session_id: owner, language_id: "EN".into() };
    let c = Client::new(Conn::new(stream, addr), Arc::new(Config::default()), db.clone(), sde, crate::chat::Hub::new("localhost"), world.clone(), login);
    let stats = ship_stats(&db, &world, system, owner, ship).await.ok().unwrap();
    let before = dogma::overrides(&db, ship).await.unwrap().len();
    set_damage(&c, system, ship, owner + 1, [false, true, true], 0.5).await.ok().unwrap();
    assert!(!state().condition.contains_key(&ship));
    assert_eq!(dogma::overrides(&db, ship).await.unwrap().len(), before);
    set_damage(&c, system, ship, owner, [false, true, true], 0.5).await.ok().unwrap();
    let rows = dogma::overrides(&db, ship).await.unwrap();
    for (id, max) in [(ATTR_ARMOR_DAMAGE, stats.max[1]), (ATTR_HULL_DAMAGE, stats.max[2])] {
        assert!((rows.iter().find(|r| r.attribute_id == id).unwrap().value - max * 0.5).abs() < 1e-9);
    }
    invalidate_condition(ship);
    let health = load_space_condition(&db, &world, system, owner, ship).await;
    assert!((health[1] - 0.5).abs() < 1e-9 && (health[2] - 0.5).abs() < 1e-9);
    save_condition(&db, ship).await.ok().unwrap();
    invalidate_condition(ship);
    let health = load_space_condition(&db, &world, system, owner, ship).await;
    assert!((health[1] - 0.5).abs() < 1e-9 && (health[2] - 0.5).abs() < 1e-9);
    gm_repair(&db, &world, system, owner, ship).await.ok().unwrap();
    let rows = dogma::overrides(&db, ship).await.unwrap();
    for id in [ATTR_ARMOR_DAMAGE, ATTR_HULL_DAMAGE] {
        assert_eq!(rows.iter().find(|r| r.attribute_id == id).unwrap().value, 0.0);
    }
    let health = load_space_condition(&db, &world, system, owner, ship).await;
    assert_eq!(health, [1.0; 3]);
    invalidate_condition(ship);
    world.remove_ball(system, ship).await;
    db.execute("DELETE FROM game.item_attributes WHERE item_id = $1", &[&ship]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id = $1", &[&ship]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE character_id = $1", &[&owner]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&account]).await.unwrap();
}
