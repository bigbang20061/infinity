use super::*;
use crate::space::System;

fn ball() -> Ball {
    Ball {
        id: 9_500_000_000_000, type_id: 1, owner_id: 1, corp_id: 1, alliance_id: -1,
        char_id: None, signature_radius: None, mode: crate::space::MODE_STOP, radius: 1.0, mass: 1.0,
        max_vel: 1.0, agility: 1.0, speed_fraction: 0.0,
        pos: [1.0, 2.0, 3.0], vel: [0.0; 3], goto: [0.0; 3],
        old_pos: [1.0, 2.0, 3.0], old_vel: [0.0; 3], segment_acceleration: None,
        follow_id: 0, follow_range: 0.0, free: true, global: false, interactive: false,
        massive: true, warp: crate::space::WarpState::default(), warp_factor: 0, last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0,
        cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new(),
    }
}

#[test]
fn missile_lifecycle_preserves_exploration_site() {
    let mut system = System::new(0, Arc::default(), 12_345);
    let site = ball();
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
fn hit_quality_bands() {
    assert_eq!(hit_quality(0.0), 0);
    assert_eq!(hit_quality(0.5), 1);
    assert_eq!(hit_quality(1.1), 4);
    assert_eq!(hit_quality(3.0), 6);
}


#[test]
fn turret_native_accuracy_vectors_and_guards() {
    let mut src = ball();
    src.id = 1;
    src.pos = [0.0; 3];
    src.radius = 100.0;
    let mut dst = ball();
    dst.id = 2;
    dst.radius = 10.0;
    dst.pos = [10_000.0, 0.0, 0.0];
    dst.vel = [0.0; 3];
    let chance = |a: &Ball, b: &Ball, optimal, falloff, tracking| {
        turret_chance(a, b, optimal, falloff, tracking, 400.0, 40.0)
    };
    assert_eq!(chance(&src, &dst, 20_000.0, 10_000.0, 0.01), 1.0);
    dst.vel = [500.0, 0.0, 0.0];
    assert_eq!(chance(&src, &dst, 20_000.0, 10_000.0, 0.01), 1.0);
    dst.vel = [0.0, 10.0, 0.0];
    assert!((chance(&src, &dst, 20_000.0, 10_000.0, 0.01) - 0.5).abs() < 1e-12);
    dst.vel = [0.0, 500.0, 0.0];
    assert!(chance(&src, &dst, 20_000.0, 10_000.0, 0.01) < 0.2);
    dst.vel = [0.0; 3];
    dst.pos[0] = 30_110.0;
    assert!((chance(&src, &dst, 20_000.0, 10_000.0, 0.01) - 0.5).abs() < 1e-12);
    dst.vel = [0.0, 30.11, 0.0];
    assert!((chance(&src, &dst, 20_000.0, 10_000.0, 0.01) - 0.25).abs() < 1e-12);
    assert!((chance(&src, &dst, 20_000.0, -10_000.0, -0.01) - 0.25).abs() < 1e-12);
    assert!((chance(&src, &dst, 20_000.0, -10_000.0, 0.01) - 0.25).abs() < 1e-12);
    assert!((chance(&src, &dst, 20_000.0, 10_000.0, -0.01) - 0.25).abs() < 1e-12);
    assert_eq!(chance(&src, &dst, 20_000.0, 0.0, 0.0), 0.0);
    assert_eq!(chance(&src, &dst, 40_000.0, 0.0, 0.0), 1.0);
    assert_eq!(chance(&src, &dst, -1.0, 10_000.0, 0.01), 0.0);
    dst.id = src.id;
    assert_eq!(chance(&src, &dst, 20_000.0, 10_000.0, 0.01), 0.0);
    dst.id = 2;
    dst.pos = src.pos;
    assert_eq!(chance(&src, &dst, 20_000.0, 10_000.0, 0.01), 0.0);
}

#[test]
fn missile_application_vectors() {
    // Scourge Light: sig_f=35/40=.875; vel_f=(.875*170/400)^.604 ≈ .550.
    let expected = (0.875_f64 * 170.0 / 400.0).powf(0.604);
    assert!((missile_applied(35.0, 400.0, 40.0, 170.0, 0.604) - expected).abs() < 1e-12);
    assert!((expected - 0.550).abs() < 0.001);
    assert_eq!(missile_applied(35.0, 0.0, 40.0, 170.0, 0.604), 0.875);
    assert_eq!(missile_applied(35.0, 400.0, 40.0, 0.0, 0.604), 0.875);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated test database with SDE"]
async fn signature_uses_cached_type_and_default_values() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Sde::load(&db).await.unwrap();
    let data = sde.types.iter().find(|t| t.attribute(ATTR_SIGNATURE_RADIUS).is_some_and(|v| v > 0.0))
        .expect("SDE type with signature radius");
    let mut target = ball();
    target.type_id = data.type_id;
    target.signature_radius = Some(0.0);
    assert_eq!(target_signature(&sde, &target), data.attribute(ATTR_SIGNATURE_RADIUS).unwrap());
    target.signature_radius = Some(42.0);
    assert_eq!(target_signature(&sde, &target), 42.0);
    target.type_id = -1;
    target.signature_radius = Some(0.0);
    assert_eq!(target_signature(&sde, &target), 100.0);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated test database with SDE"]
async fn missile_uses_signature_and_speed_at_impact() {
    use crate::consts::{ATTR_SHIELD_CAPACITY, ATTR_SHIELD_EM_DAMAGE_RESONANCE, CATEGORY_ENTITY};
    use crate::services::combat::groups_health::npc_health_fraction;

    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let data = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_ENTITY)
        && t.attribute(ATTR_SHIELD_CAPACITY).is_some_and(|v| v.is_finite() && v > 0.0)
        && t.attribute(ATTR_SHIELD_EM_DAMAGE_RESONANCE).is_some_and(|v| v.is_finite() && v > 0.0))
        .expect("SDE NPC with shield and EM resonance");
    let (type_id, shield, resonance) = (data.type_id, data.attribute(ATTR_SHIELD_CAPACITY).unwrap(),
        data.attribute(ATTR_SHIELD_EM_DAMAGE_RESONANCE).unwrap());
    let charge = sde.types.iter().next().expect("SDE charge fixture type").type_id;
    let world = World::new(db.clone(), sde, 1);
    let system = -31_001;
    let target_id = -NEXT_MISSILE_ID.fetch_add(1, Ordering::Relaxed);
    let mut source = ball();
    source.id = target_id - 1;
    source.pos = [0.0; 3];
    let mut target = ball();
    target.id = target_id;
    target.type_id = type_id;
    target.mode = crate::space::MODE_RIGID;
    target.free = false;
    target.pos = [2_000.0, 0.0, 0.0];
    target.signature_radius = Some(200.0);
    target.vel = [0.0; 3];
    world.system(system).await.unwrap();
    let inserted = target.clone();
    world.with(system, move |s| { s.insert(inserted); }).await.unwrap();
    let stale = Missile {
        charge, raw: [shield, 0.0, 0.0, 0.0], aoe_cloud: 40.0, aoe_velocity: 170.0,
        drf: 0.604, velocity: 1_000.0, flight_range: 10_000.0, weapon: charge, attacker: None,
    };
    let before = world.with(system, |s| s.balls().count()).await.unwrap();
    let guard = crate::services::effects::space_source::SourceGuard::Npc {
        source: source.id, target: target.id, generation: 0,
    };
    launch_missile(db.clone(), world.clone(), system, source.clone(), target.clone(), 1, None, stale, Some(guard)).await.ok().unwrap();
    assert_eq!(world.with(system, |s| s.balls().count()).await.unwrap(), before);
    let missile = Missile {
        charge, raw: [shield * 0.25 / resonance, 0.0, 0.0, 0.0],
        aoe_cloud: 40.0, aoe_velocity: 170.0, drf: 0.604,
        velocity: 1_000.0, flight_range: 10_000.0, weapon: charge, attacker: None,
    };
    launch_missile(db, world.clone(), system, source, target, 1, None, missile, None).await.ok().unwrap();
    world.with(system, move |s| s.command(target_id, |ball, _, _| {
        ball.signature_radius = Some(35.0);
        ball.vel = [0.0, 400.0, 0.0];
        ball.old_vel = ball.vel;
        ball.segment_acceleration = None;
        vec![]
    })).await.unwrap();
    assert!(world.after(system, 3_000).await);
    let fraction = world.with(system, move |s| npc_health_fraction(s.ball(target_id).unwrap(), 0).unwrap()).await.unwrap();
    let application = (0.875_f64 * 170.0 / 400.0).powf(0.604);
    assert!((fraction - (1.0 - 0.25 * application)).abs() < 1e-9);
    crate::services::combat::forget_ship(target_id);
    world.remove_ball(system, target_id).await;
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated test database with SDE"]
async fn npc_miss_reports_physical_source_to_drone_owner() {
    use crate::space::drone::DroneSpec;
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let world = World::new(db.clone(), sde, 1);
    let system = -31_002;
    let mut source = ball();
    source.id = -31_003;
    source.pos = [0.0; 3];
    let mut target = ball();
    target.id = -31_004;
    target.owner_id = -31_005;
    target.pos = source.pos;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(target.owner_id, tx);
    world.system(system).await.unwrap();
    let (a, b) = (source.clone(), target.clone());
    world.with(system, move |s| {
        s.insert(a);
        s.restore_drone(b, DroneSpec { type_id: 1, bandwidth: 0.0, scan_resolution: 0.0,
            control_range: 0.0, max_target_range: 0.0, optimal_range: 0.0, orbit_range: 0.0,
            duration_ms: 0.0, effect_id: None }, Arc::new(|_| Box::pin(async {})));
    }).await.unwrap();
    assert_eq!(target.char_id, None);
    turret_shot(&db, &world, system, &source, &target, None, 42, (1.0, 1.0, 1.0, 1.0), [1.0; 4], None).await.ok().unwrap();
    let (event, args) = rx.try_recv().expect("drone owner receives NPC hit log");
    assert_eq!(event, "OnDamageMessage");
    assert_eq!(args[0], damage_message("source", source.id, 42, 0.0, HIT_MISS));
    world.unregister_pilot(target.owner_id);
    world.remove_ball(system, target.id).await;
    world.remove_ball(system, source.id).await;
}
