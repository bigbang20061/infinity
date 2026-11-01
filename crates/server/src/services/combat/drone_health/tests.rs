use super::*;

#[test]
fn refresh_preserves_absolute_damage_and_updates_resonance() {
    let mut hp = Hp { now: [80.0, 100.0, 150.0], max: [100.0, 200.0, 300.0], resonance: [[1.0; 4]; 3] };
    let stats = ShipStats { max: [120.0, 400.0, 100.0], resonance: [[0.5; 4]; 3], shield_tau_ms: 0.0 };
    rebase(&mut hp, &stats);
    assert_eq!(hp.now, [100.0, 300.0, 0.0]);
    assert_eq!(hp.resonance, stats.resonance);
}

#[test]
fn saved_damage_restores_against_effective_maxima() {
    let stats = ShipStats { max: [100.0, 200.0, 300.0], resonance: [[1.0; 4]; 3], shield_tau_ms: 0.0 };
    let (now, fractions) = restored_condition(&stats, [Some(80.0), Some(30.0), Some(40.0)], 0, 0);
    assert_eq!(now, [80.0, 170.0, 260.0]);
    assert_eq!(HealthSeed(Hp { now, max: stats.max, resonance: stats.resonance }).fractions(), fractions);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn indexed_drone_repair_uses_shared_health_cache() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let type_id = sde.types.iter().find(|t| t.category_id == Some(crate::consts::CATEGORY_DRONE)).unwrap().type_id;
    let id = i64::MIN + 7311;
    let ball = Ball {
        id, type_id, owner_id: 1, corp_id: 1, alliance_id: -1, char_id: None,
        signature_radius: None, mode: crate::space::MODE_STOP, radius: 1.0, mass: 1.0,
        max_vel: 0.0, agility: 1.0, speed_fraction: 0.0, pos: [0.0; 3], vel: [0.0; 3],
        old_pos: [0.0; 3], old_vel: [0.0; 3], segment_acceleration: None, goto: [0.0; 3],
        follow_id: 0, follow_range: 0.0, free: true, global: false, massive: true,
        interactive: false, warp: crate::space::WarpState::default(), warp_factor: 0, last_ms: 0,
        interpolation_anchor_ms: 0, effect_stamp: 0, cloak: 0, cloak_until: 0,
        health: [0.5; 3], launcher: 0, slim: Vec::new(),
    };
    let spec = crate::space::drone::DroneSpec { type_id, bandwidth: 0.0, scan_resolution: 0.0,
        control_range: 0.0, max_target_range: 0.0, optimal_range: 0.0, orbit_range: 0.0,
        duration_ms: 0.0, effect_id: None };
    let mut system = crate::space::System::default();
    system.restore_drone(ball, spec, Arc::new(|_| Box::pin(async {})));
    install(id, HealthSeed(Hp { now: [50.0; 3], max: [100.0; 3], resonance: [[1.0; 4]; 3] }));
    assert!(super::super::groups_health::repair_npc_in_system(&mut system, &sde, id, 1, 25.0));
    assert_eq!(state().hp.get(&id).unwrap().now, [50.0, 75.0, 50.0]);
    state().hp.remove(&id);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn condition_save_and_guarded_death_do_not_resurrect_items() {
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let owner = 101;
    let system = 103;
    let id = crate::dao::items::creation::positioned(&tx, 1, owner, system, [1.0; 3]).await.unwrap();
    install(id, HealthSeed(Hp { now: [80.0, 70.0, 60.0], max: [100.0; 3], resonance: [[1.0; 4]; 3] }));
    save_condition_in(&tx, id).await.ok().unwrap();
    let rows = crate::dao::items::dogma::overrides(&tx, id).await.unwrap();
    let value = |attribute| rows.iter().find(|r| r.attribute_id == attribute).map(|r| r.value);
    assert_eq!([value(ATTR_SHIELD_CHARGE), value(ATTR_ARMOR_DAMAGE), value(ATTR_HULL_DAMAGE)], [Some(80.0), Some(30.0), Some(40.0)]);
    assert!(crate::dao::items::drones::destroy(&tx, id, owner, system).await.unwrap());
    assert!(!crate::dao::items::drones::return_to_bay(&tx, id, owner, system, owner, 102).await.unwrap());
    save_condition_in(&tx, id).await.ok().unwrap();
    assert!(crate::dao::items::dogma::overrides(&tx, id).await.unwrap().is_empty());
    let survivor = crate::dao::items::creation::positioned(&tx, 1, owner, system, [1.0; 3]).await.unwrap();
    assert!(crate::dao::items::drones::return_to_bay(&tx, survivor, owner, system, owner, 102).await.unwrap());
    assert!(!crate::dao::items::drones::destroy(&tx, survivor, owner, system).await.unwrap());
    state().hp.remove(&id);
    tx.rollback().await.unwrap();
}
