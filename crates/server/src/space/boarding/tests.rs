use super::*;
use crate::space::WarpState;

fn ball(id: i64, kind: i32, pilot: Option<i64>, x: f64) -> Ball {
    Ball {
        id, type_id: kind, owner_id: 1, corp_id: 1, alliance_id: -1,
        char_id: pilot, signature_radius: Some(1.0), mode: MODE_STOP, radius: 1.0,
        mass: 1e6, max_vel: 100.0, agility: 1.0, speed_fraction: 0.0,
        pos: [x, 0.0, 0.0], vel: [0.0; 3], old_pos: [x, 0.0, 0.0],
        old_vel: [0.0; 3], segment_acceleration: None, goto: [0.0; 3],
        follow_id: 0, follow_range: 0.0, free: true, global: false,
        massive: true, interactive: pilot.is_some(), warp: WarpState::default(),
        warp_factor: 0, last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0,
        cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new(),
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn authorization_checks_identity_category_occupancy_safety_and_surface_range() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::deployment::DatabaseConfig {
        schema_on_start: false, ..crate::config::tests::test_database()
    }).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let ship = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
    let other = sde.types.iter().find(|t| t.category_id != Some(CATEGORY_SHIP)).unwrap().type_id;
    let mut s = System::new(1, Arc::default(), 1000);
    s.projection_sde = Some(sde);
    s.insert(ball(10, ship, Some(1), 0.0));
    s.add_persistent_ship(ball(20, ship, None, 12.0));
    let snapshot = s.authorize_board(1, 10, 20, 10.0).ok().unwrap();
    assert_eq!(snapshot.old.pos, [0.0; 3]);
    assert_eq!(snapshot.target.pos, [12.0, 0.0, 0.0]);
    assert!(matches!(s.authorize_board(1, 10, 20, 9.999), Err(BoardRefusal::OutOfRange)));
    assert!(matches!(s.authorize_board(1, 20, 10, 10.0), Err(BoardRefusal::StalePilot)));
    assert!(matches!(s.authorize_board(1, 10, 10, 10.0), Err(BoardRefusal::InvalidTarget)));
    s.balls.get_mut(&20).unwrap().owner_id = 2;
    assert!(matches!(s.authorize_board(1, 10, 20, 10.0), Err(BoardRefusal::InvalidTarget)));
    s.balls.get_mut(&20).unwrap().owner_id = 1;
    s.balls.get_mut(&20).unwrap().type_id = other;
    assert!(matches!(s.authorize_board(1, 10, 20, 10.0), Err(BoardRefusal::InvalidTarget)));
    s.balls.get_mut(&20).unwrap().type_id = ship;
    s.balls.get_mut(&20).unwrap().interactive = true;
    assert!(matches!(s.authorize_board(1, 10, 20, 10.0), Err(BoardRefusal::Occupied)));
    s.balls.get_mut(&20).unwrap().interactive = false;
    s.balls.get_mut(&20).unwrap().char_id = Some(2);
    assert!(matches!(s.authorize_board(1, 10, 20, 10.0), Err(BoardRefusal::Occupied)));
    s.balls.get_mut(&20).unwrap().char_id = None;
    s.balls.get_mut(&20).unwrap().cloak = 1;
    assert!(matches!(s.authorize_board(1, 10, 20, 10.0), Err(BoardRefusal::Unsafe)));
    s.balls.get_mut(&20).unwrap().cloak = 0;
    s.balls.get_mut(&10).unwrap().mode = MODE_WARP;
    assert!(matches!(s.authorize_board(1, 10, 20, 10.0), Err(BoardRefusal::Unsafe)));
    s.balls.get_mut(&10).unwrap().mode = MODE_STOP;
    s.balls.get_mut(&20).unwrap().radius = f32::NAN;
    assert!(matches!(s.authorize_board(1, 10, 20, 10.0), Err(BoardRefusal::Unsafe)));
    s.balls.get_mut(&20).unwrap().radius = 1.0;
    s.take(20);
    s.add_persistent_ship(ball(20, ship, None, 0.0));
    assert_ne!(snapshot.target_generation, s.persistent_generation(20).unwrap());
    assert!(s.authorize_board(1, 10, 20, 0.0).is_ok());
    s.balls.get_mut(&10).unwrap().vel = [10.0, 0.0, 0.0];
    s.tick(2000);
    s.tick(2500);
    let sampled = s.authorize_board(1, 10, 20, 100.0).ok().unwrap();
    assert_eq!(sampled.old.pos, s.sample_position(10).unwrap());
    assert_ne!(sampled.old.pos, s.ball(10).unwrap().pos);
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn committed_publication_replaces_membership_and_preserves_observer() {
    use crate::dao::items::{creation, space_ships::FreeShip};
    use crate::services::{combat, ship::balls};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::deployment::DatabaseConfig {
        schema_on_start: false, ..crate::config::tests::test_database()
    }).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let kind = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
    let station = sde.map.stations().next().unwrap().station_id;
    let (account, owner, old_id, target_id) = db.transaction(
        |tx, _: &mut ()| Box::pin(async move {
            let account: i64 = tx.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES (nextval('game.accounts_user_id_seq')::text, ''::bytea, 0) RETURNING user_id", &[]).await?.get("user_id");
            let owner: i64 = tx.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id, station_id, balance) VALUES ($1, nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1, $2, 0) RETURNING character_id", &[&account, &station]).await?.get("character_id");
            let old = creation::positioned(tx, kind, owner, station, [0.0; 3]).await?;
            let target = creation::positioned(tx, kind, owner, station, [2.0, 0.0, 0.0]).await?;
            Ok::<_, crate::db::Error>((account, owner, old, target))
        }), |value, ()| async move { Ok(value) }).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
    let db = test_db;
    let row = |id, pos| FreeShip { item_id: id, type_id: kind, owner_id: owner, corporation_id: 1, alliance_id: -1, pos };
    let old = balls::unpiloted(&db, sde.clone(), &row(old_id, [0.0; 3])).await.ok().unwrap();
    let mut target = balls::unpiloted(&db, sde.clone(), &row(target_id, [2.0, 0.0, 0.0])).await.ok().unwrap();
    target.ball.char_id = Some(owner);
    target.ball.interactive = true;
    let mut s = System::new(1, Arc::default(), 1000);
    s.projection_sde = Some(sde);
    let mut original = old.ball.clone();
    original.char_id = Some(owner);
    original.interactive = true;
    s.insert(original);
    let mut free_target = target.ball.clone();
    free_target.char_id = None;
    free_target.interactive = false;
    s.add_persistent_ship(free_target);
    let (tx, _rx) = mpsc::unbounded_channel();
    s.observers.insert(7, (old_id, tx));
    let snapshot = s.authorize_board(owner, old_id, target_id, 100.0).ok().unwrap();
    let old_generation = snapshot.old_generation;
    let target_generation = snapshot.target_generation;
    s.compressors.insert(old_id, HashMap::new());
    let queued = s.actions.len();
    s.apply_committed_board(7, owner, snapshot, old, target);
    assert!(!s.compressors.contains_key(&old_id));
    assert_eq!(s.actions.len(), queued + 4);
    assert_eq!(s.pilot_ball(owner).unwrap().id, target_id);
    assert_eq!(s.ball(old_id).unwrap().char_id, None);
    assert!(!s.ball(old_id).unwrap().interactive);
    assert_eq!(s.observers[&7].0, target_id);
    assert_ne!(s.persistent_generation(old_id), Some(old_generation));
    assert_ne!(s.persistent_generation(target_id), Some(target_generation));
    assert_eq!(s.ball(old_id).unwrap().pos, [0.0; 3]);
    assert_eq!(s.ball(target_id).unwrap().pos, [2.0, 0.0, 0.0]);
    assert_eq!(s.ball(target_id).unwrap().vel, [0.0; 3]);
    }).await;
    combat::forget_ship(old_id);
    combat::forget_ship(target_id);
    db.execute("DELETE FROM game.item_attributes WHERE item_id = ANY($1)", &[&vec![old_id, target_id]]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id = ANY($1)", &[&vec![old_id, target_id]]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE character_id = $1", &[&owner]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&account]).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn stopped_actor_recovers_committed_boarding_from_loader_and_seeds() {
    use crate::dao::items::{boarding, creation, space_ships::FreeShip};
    use crate::services::{combat, ship::balls};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::deployment::DatabaseConfig {
        schema_on_start: false, ..crate::config::tests::test_database()
    }).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let kind = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
    let system = sde.map.systems().next().unwrap().solar_system_id;
    let old_pos = [1.0e12, 1.0e12, 1.0e12];
    let target_pos = [1.0e12 + 2.0, 1.0e12, 1.0e12];
    let (account, owner, old_id, target_id) = db.transaction(
        |tx, _: &mut ()| Box::pin(async move {
            let account: i64 = tx.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES (nextval('game.accounts_user_id_seq')::text, ''::bytea, 0) RETURNING user_id", &[]).await?.get("user_id");
            let owner: i64 = tx.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id, station_id, solar_system_id, balance) VALUES ($1, nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1, NULL, $2, 0) RETURNING character_id", &[&account, &system]).await?.get("character_id");
            let old = creation::positioned(tx, kind, owner, system, old_pos).await?;
            let target = creation::positioned(tx, kind, owner, system, target_pos).await?;
            tx.execute("UPDATE game.characters SET ship_id=$2 WHERE character_id=$1", &[&owner, &old]).await?;
            Ok::<_, crate::db::Error>((account, owner, old, target))
        }), |value, ()| async move { Ok(value) }).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let db = test_db;
        let world = World::new(db.clone(), sde.clone(), 1);
        let row = |id, pos| FreeShip { item_id: id, type_id: kind, owner_id: owner, corporation_id: 1, alliance_id: -1, pos };
        let old = balls::unpiloted(&db, sde.clone(), &row(old_id, old_pos)).await.ok().unwrap();
        let target = balls::piloted(&db, sde.clone(), &row(target_id, target_pos), owner, 0.0).await.ok().unwrap();
        world.system(system).await.unwrap();
        let original = balls::piloted(&db, sde.clone(), &row(old_id, old_pos), owner, 0.0).await.ok().unwrap().ball;
        world.add_ship(system, original).await.unwrap();
        let snapshot = world.with(system, move |s| s.authorize_board(owner, old_id, target_id, 1.0e12))
            .await.unwrap().ok().unwrap();
        let committed = db.transaction(
            |tx, _: &mut ()| Box::pin(async move {
                boarding::board_in_space(tx, owner, old_id, target_id, system, kind, kind, old_pos, target_pos).await
            }), |value, ()| async move { Ok(value) }).await.unwrap();
        assert!(committed);
        let selected: i64 = db.query_one("SELECT ship_id FROM game.characters WHERE character_id=$1", &[&owner]).await.unwrap().get("ship_id");
        assert_eq!(selected, target_id);
        let restored = world.restore_persistent_ships(system).await.unwrap();
        assert!(restored.iter().any(|ship| ship.ball.id == old_id));
        assert!(!restored.iter().any(|ship| ship.ball.id == target_id));
        drop(restored);

        // The actor's snapshot context owns this gate until the System is dropped.
        let handle = world.systems.lock().unwrap().remove(&system).unwrap();
        let stopped = Arc::downgrade(&handle.gate);
        drop(handle);
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while stopped.upgrade().is_some() { tokio::task::yield_now().await; }
        }).await.expect("actor did not stop");
        world.apply_committed_board(system, 7, owner, snapshot, old, target).await.unwrap();
        let (tx, _rx) = mpsc::unbounded_channel();
        world.observe(system, 7, target_id, tx).await.unwrap();
        world.with(system, move |s| {
            assert_eq!(s.pilots().filter(|b| b.char_id == Some(owner)).count(), 1);
            assert_eq!(s.pilot_ball(owner).unwrap().id, target_id);
            assert_eq!(s.ball(old_id).unwrap().char_id, None);
            assert!(!s.ball(old_id).unwrap().interactive);
            assert!(s.persistent_generation(old_id).is_some());
            assert!(s.persistent_generation(target_id).is_some());
            assert_eq!(s.observers[&7].0, target_id);
        }).await.unwrap();
    }).await;
    combat::forget_ship(old_id);
    combat::forget_ship(target_id);
    db.execute("DELETE FROM game.item_attributes WHERE item_id = ANY($1)", &[&vec![old_id, target_id]]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id = ANY($1)", &[&vec![old_id, target_id]]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE character_id = $1", &[&owner]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&account]).await.unwrap();
    result.unwrap();
}
