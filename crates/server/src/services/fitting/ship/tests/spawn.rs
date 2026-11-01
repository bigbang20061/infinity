//! Real-database coverage for spawning and restoring unpiloted hulls.
use std::sync::Arc;
use crate::{client::Client, consts::CATEGORY_SHIP, db::Db, sde::Sde, space::World};
use marshal::Value;

async fn fingerprint(db: &Db) -> Vec<(String, i64, Vec<(String, String)>)> {
    let mut result = Vec::new();
    for (table, key) in [("accounts", "user_id::text"), ("characters", "character_id::text"),
        ("items", "item_id::text"), ("item_attributes", "item_id::text || ':' || attribute_id::text")] {
        let count = db.query_one(&format!("SELECT count(*) AS n FROM game.{table}"), &[]).await.unwrap().get("n");
        let rows = db.query(&format!("SELECT {key} AS key, md5(row_to_json(t)::text) AS hash FROM game.{table} t ORDER BY key"), &[]).await.unwrap()
            .iter().map(|row| (row.get("key"), row.get("hash"))).collect();
        result.push((table.to_owned(), count, rows));
    }
    result
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn spawn_preserves_active_ship_and_restores_one_unpiloted_hull() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::deployment::DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    // Keep the later fixed move safely outside the pilot's collision sphere.
    const MOVE: [f64; 3] = [12345.0, 23456.0, 34567.0];
    const MARGIN: f64 = 10_000.0;
    let move_distance = MOVE.iter().map(|v| v * v).sum::<f64>().sqrt();
    let (kind, radius) = sde.types.iter()
        .filter(|t| t.category_id == Some(CATEGORY_SHIP) && t.published == Some(true))
        .filter_map(|t| crate::services::ship::balls::radius(&sde, t.type_id).ok()
            .map(|radius| (t.type_id, f64::from(radius))))
        .filter(|(_, radius)| 2.0 * radius + MARGIN < move_distance)
        .min_by_key(|(kind, _)| *kind).expect("no suitable published SDE ship");
    let world = World::new(db.clone(), sde.clone(), 1);
    let mut systems: Vec<_> = sde.map.systems().map(|s| s.solar_system_id).collect();
    systems.sort_unstable();
    let mut placement = None;
    for system in systems {
        world.system(system).await.unwrap();
        // The clearance covers every random spawn direction, the later move, both
        // hull radii, and an additional 10 km test margin.
        let clearance = crate::config::game().gm.spawn_distance_m + 2.0 * radius
            + move_distance + MARGIN;
        let initial = world.with(system, move |s| {
            let mut best: Option<[f64; 3]> = None;
            for axis in 0..3 {
                for sign in [-1.0, 1.0] {
                    let mut distance = 0.0_f64;
                    loop {
                        let mut next = distance;
                        for ball in s.balls() {
                            let pos = s.sample_position(ball.id).unwrap_or(ball.pos);
                            let expanded = f64::from(ball.radius) + clearance;
                            if (0..3).filter(|&i| i != axis).all(|i| pos[i].abs() <= expanded) {
                                let center = sign * pos[axis];
                                if distance >= center - expanded && distance <= center + expanded {
                                    next = next.max(center + expanded + 1.0);
                                }
                            }
                        }
                        if next == distance { break; }
                        distance = next;
                        // Below 2^29 metres the f64 spacing is under 0.12 micrometres.
                        if distance >= 536_870_912.0 { break; }
                    }
                    if distance < 536_870_912.0
                        && best.is_none_or(|p| distance < p.iter().map(|v| v.abs()).fold(0.0_f64, f64::max))
                    {
                        let mut pos = [0.0; 3];
                        pos[axis] = sign * distance;
                        best = Some(pos);
                    }
                }
            }
            if let Some(pos) = best {
                assert!(s.balls().all(|ball| {
                    let center = s.sample_position(ball.id).unwrap_or(ball.pos);
                    crate::space::dist(pos, center) > f64::from(ball.radius) + clearance
                }), "fixture placement must clear every actor ball");
            }
            best
        }).await.unwrap();
        if let Some(initial) = initial {
            placement = Some((system, initial));
            break;
        }
    }
    let (system, initial) = placement.expect("no precision-safe collision-free SDE system found");
    let before = fingerprint(&db).await;
    let (account, owner, active): (i64, i64, i64) = db.transaction(|tx, _: &mut ()| Box::pin(async move {
        let account = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ('spawn-fixture-' || nextval('game.accounts_user_id_seq')::text,$1,0) RETURNING user_id", &[&&b"fixture"[..]]).await?.get("user_id");
        let owner = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id,solar_system_id,station_id) VALUES ($1,'spawn-fixture-' || nextval('game.characters_character_id_seq')::text,1,1,1,1,$2,NULL) RETURNING character_id", &[&account,&system]).await?.get("character_id");
        let active = crate::dao::items::creation::positioned(tx, kind, owner, system, [0.0; 3]).await?;
        tx.execute("UPDATE game.characters SET ship_id=$2 WHERE character_id=$1", &[&owner,&active]).await?;
        Ok::<_, crate::db::Error>((account, owner, active))
    }), |v, ()| async move { Ok(v) }).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let row = crate::dao::items::space_ships::FreeShip { item_id: active, type_id: kind, owner_id: owner, corporation_id: 1, alliance_id: 0, pos: initial };
        let mut pilot = crate::services::ship::balls::unpiloted(&test_db, sde.clone(), &row).await.unwrap().install();
        pilot.char_id = Some(owner);
        pilot.interactive = true;
        world.add_ship(system, pilot).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, address) = listener.accept().await.unwrap();
        let mut c = Client::new(crate::conn::Conn::new(stream, address), Arc::new(crate::config::Config::default()), test_db.clone(), sde.clone(), crate::chat::Hub::new("localhost", test_db.clone(), world.clone()), world.clone(), crate::handshake::Login { user_id: account, user_name: "spawn-fixture".into(), role: 0, client_id: 1, session_id: 1, language_id: "EN".into() });
        c.session.apply(vec![("charid", Value::Int(owner)), ("shipid", Value::Int(active)), ("solarsystemid", Value::Int(system))]);
        let id = super::super::gm_spawn_ship(&mut c, kind).await.unwrap();
        assert_eq!(c.session.int("shipid"), Some(active));
        let row = test_db.query_one("SELECT owner_id,location_id,flag_id,singleton,quantity,x,y,z FROM game.items WHERE item_id=$1", &[&id]).await.unwrap();
        assert_eq!(row.get::<_, i64>("owner_id"), owner);
        assert_eq!(row.get::<_, i64>("location_id"), system);
        assert_eq!(row.get::<_, i32>("flag_id"), 0);
        assert!(row.get::<_, bool>("singleton"));
        assert_eq!(row.get::<_, i32>("quantity"), 1);
        assert!(row.get::<_, Option<f64>>("x").is_some());
        let ball = world.ball(system, id).await.unwrap();
        assert_eq!(ball.char_id, None);
        let pilot = world.ball(system, active).await.unwrap();
        let separation = ((0..3).map(|i| (ball.pos[i] - pilot.pos[i]).powi(2)).sum::<f64>()).sqrt();
        assert!((separation - (crate::config::game().gm.spawn_distance_m + f64::from(ball.radius) + f64::from(pilot.radius))).abs() < 1e-6);
        assert_eq!(test_db.query_one("SELECT ship_id FROM game.characters WHERE character_id=$1", &[&owner]).await.unwrap().get::<_, Option<i64>>("ship_id"), Some(active));
        assert!(!ball.interactive);
        let generation = world.with(system, move |s| s.persistent_generation(id)).await.unwrap().unwrap();
        let duplicate_row = crate::dao::items::space_ships::FreeShip { item_id: id, type_id: kind, owner_id: owner, corporation_id: ball.corp_id, alliance_id: ball.alliance_id, pos: ball.pos };
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let duplicate_task = {
            let db = test_db.clone();
            let sde = sde.clone();
            let world = world.clone();
            let barrier = barrier.clone();
            tokio::spawn(async move {
                let duplicate = crate::services::ship::balls::unpiloted(&db, sde, &duplicate_row).await.unwrap();
                barrier.wait().await;
                barrier.wait().await;
                world.add_persistent_ship(system, duplicate).await.unwrap();
            })
        };
        barrier.wait().await;
        assert!(crate::services::combat::unpiloted::damage(&test_db, &world, system, id, [1.0, 0.0, 0.0, 0.0]).await.unwrap().unwrap() > 0.0);
        let damaged = world.ball(system, id).await.unwrap();
        let cached = crate::services::combat::groups_health::npc_health_fraction(&damaged, 0);
        barrier.wait().await;
        duplicate_task.await.unwrap();
        assert_eq!(crate::services::combat::groups_health::npc_health_fraction(&damaged, 0), cached);
        assert_eq!(world.with(system, move |s| s.persistent_generation(id)).await.unwrap(), Some(generation));
        let restored = World::new(test_db.clone(), sde, 1);
        let moved = [initial[0] + MOVE[0], initial[1] + MOVE[1], initial[2] + MOVE[2]];
        world.with(system, move |s| s.command(id, move |b, _, _| { b.pos = moved; Vec::new() })).await.unwrap();
        assert!(world.after(system, 1100).await);
        world.flush_positions().await.unwrap();
        let saved = test_db.query_one("SELECT x,y,z FROM game.items WHERE item_id=$1", &[&id]).await.unwrap();
        assert_eq!([saved.get::<_, f64>("x"), saved.get("y"), saved.get("z")], moved);
        assert!(crate::services::combat::unpiloted::damage(&test_db, &world, system, id, [1.0, 0.0, 0.0, 0.0]).await.unwrap().unwrap() > 0.0);
        restored.system(system).await.unwrap();
        assert!(restored.ball(system, id).await.is_some());
        assert_eq!(restored.with(system, move |s| s.persistent_balls().filter(|b| b.id == id).count()).await, Some(1));
        assert_eq!(restored.ball(system, id).await.unwrap().pos, moved);
        assert!(restored.ball(system, id).await.unwrap().health.iter().any(|fraction| *fraction < 1.0));
        crate::services::combat::forget_ship(id);
        crate::services::combat::forget_ship(active);
    }).await;
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        tx.execute("DELETE FROM game.item_attributes WHERE item_id IN (SELECT item_id FROM game.items WHERE owner_id=$1)", &[&owner]).await?;
        tx.execute("UPDATE game.characters SET ship_id=NULL WHERE character_id=$1", &[&owner]).await?;
        tx.execute("DELETE FROM game.items WHERE owner_id=$1", &[&owner]).await?;
        tx.execute("DELETE FROM game.characters WHERE character_id=$1", &[&owner]).await?;
        tx.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await?;
        Ok::<_, crate::db::Error>(())
    }), |(), ()| async move { Ok(()) }).await.unwrap();
    assert_eq!(fingerprint(&db).await, before);
    result.unwrap();
}
