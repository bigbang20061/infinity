//! Direct Board refusals against real game rows, SDE data and system actors.
//! Foreign custody is intentionally unsupported.
use super::*;
use crate::dao::items::{creation, space_ships::FreeShip};
use crate::services::ship::balls;
use crate::space::MODE_WARP;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn rejected_board_requests_leave_database_actor_session_and_notifications_unchanged() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::deployment::DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let system = sde.map.systems().next().expect("SDE system").solar_system_id;
    let kind = sde.types.iter().filter(|t| t.category_id == Some(CATEGORY_SHIP) && t.published == Some(true))
        .filter(|t| balls::radius(&sde, t.type_id).is_ok()).map(|t| t.type_id).min().expect("published ship");
    let skilled = sde.types.iter().filter(|t| t.category_id == Some(CATEGORY_SHIP) && t.published == Some(true))
        .filter(|t| balls::radius(&sde, t.type_id).is_ok())
        .filter(|t| ATTR_REQUIRED_SKILLS.iter().zip(ATTR_REQUIRED_SKILL_LEVELS).any(|(&skill, level)| {
            let base = |attr| crate::services::fit::base_attribute(Some(t), sde.dogma.attribute(attr), attr);
            base(skill) > 0.0 && base(level) > 0.0
        })).map(|t| t.type_id).min().expect("published ship requiring a skill");
    let nonship = sde.types.iter().find(|t| t.category_id != Some(CATEGORY_SHIP) && t.published == Some(true)).expect("published nonship").type_id;
    let before = fingerprint(&db).await;
    let (account, owner) = fixture(&db, &sde, system, None).await;
    let (foreign_account, foreign_owner) = fixture(&db, &sde, system, None).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let pos = [100_000_000.0; 3];
        let old = creation::positioned(&test_db, kind, owner, system, pos).await.unwrap();
        test_db.execute("UPDATE game.characters SET ship_id=$2 WHERE character_id=$1", &[&owner, &old]).await.unwrap();
        let world = World::new(test_db.clone(), sde.clone(), 1);
        let row = |id, type_id, at| FreeShip { item_id: id, type_id, owner_id: owner, corporation_id: 1, alliance_id: 0, pos: at };
        world.add_ship(system, balls::piloted(&test_db, sde.clone(), &row(old, kind, pos), owner, 0.0).await.unwrap().install()).await.unwrap();
        let foreign = creation::positioned(&test_db, kind, foreign_owner, system, pos).await.unwrap();
        let nonship = creation::positioned(&test_db, nonship, owner, system, pos).await.unwrap();
        let packaged = creation::positioned(&test_db, kind, owner, system, pos).await.unwrap();
        test_db.execute("UPDATE game.items SET singleton=false WHERE item_id=$1", &[&packaged]).await.unwrap();
        let occupied = creation::positioned(&test_db, kind, owner, system, pos).await.unwrap();
        let warp = creation::positioned(&test_db, kind, owner, system, pos).await.unwrap();
        let cloak = creation::positioned(&test_db, kind, owner, system, pos).await.unwrap();
        let far_pos = [200_000_000.0; 3];
        let far = creation::positioned(&test_db, kind, owner, system, far_pos).await.unwrap();
        let unskilled = creation::positioned(&test_db, skilled, owner, system, pos).await.unwrap();
        for (id, ty, at, state) in [(occupied, kind, pos, 1), (warp, kind, pos, 2), (cloak, kind, pos, 3), (far, kind, far_pos, 0), (unskilled, skilled, pos, 0)] {
            let mut ship = balls::unpiloted(&test_db, sde.clone(), &row(id, ty, at)).await.unwrap();
            match state {
                1 => ship.ball.interactive = true,
                2 => assert_eq!(ship.ball.warp_to(far_pos, 0.0, 1), crate::space::WarpOutcome::Align),
                3 => ship.ball.cloak = 1,
                _ => (),
            }
            world.add_persistent_ship(system, ship).await.unwrap();
        }
        let mut c = client(test_db.clone(), sde, world.clone(), account).await;
        c.session.apply(vec![("charid", Value::Int(owner)), ("shipid", Value::Int(old)), ("solarsystemid", Value::Int(system))]);
        let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
        world.observe(system, c.login.client_id, old, events).await.unwrap();
        assert!(world.after(system, 1_100).await);
        while received.try_recv().is_ok() {}
        let actor = || async {
            world.with(system, move |s| {
                let mut rows: Vec<_> = s.balls().filter(|b| [old, occupied, warp, cloak, far, unskilled].contains(&b.id))
                    .map(|b| (b.id, b.owner_id, b.char_id, b.interactive, b.mode, b.cloak, b.pos, b.health)).collect();
                rows.sort_by_key(|r| r.0);
                (s.pilot_ball(owner).map(|b| b.id), rows)
            }).await.unwrap()
        };
        for (label, target, context) in [("missing", i64::MAX, Some(old)), ("nonship", nonship, Some(old)),
            ("foreign", foreign, Some(old)), ("unassembled", packaged, Some(old)), ("occupied", occupied, Some(old)),
            ("warp", warp, Some(old)), ("cloak", cloak, Some(old)), ("far", far, Some(old)),
            ("stale old", unskilled, Some(old + 1)), ("missing skill", unskilled, Some(old))] {
            let snapshot = fingerprint(&test_db).await;
            let state = actor().await;
            let queued = c.outbox.len();
            match label {
                "foreign" => {
                    let row = test_db.query_one("SELECT owner_id FROM game.items WHERE item_id=$1", &[&target]).await.unwrap();
                    assert_ne!(row.get::<_, i64>("owner_id"), owner, "foreign target must have a different database owner");
                }
                "occupied" | "warp" | "cloak" | "far" => {
                    world.with(system, move |s| {
                        let ball = s.ball(target).expect("target actor ball");
                        match label {
                            "occupied" => assert!(ball.interactive || ball.char_id.is_some(), "target must be occupied"),
                            "warp" => assert_eq!(ball.mode, MODE_WARP, "target must be in warp mode"),
                            "cloak" => assert_ne!(ball.cloak, 0, "target must be cloaked"),
                            "far" => {
                                let old_ball = s.sampled_ball(old).expect("old sampled ball");
                                let target_ball = s.sampled_ball(target).expect("target sampled ball");
                                let gap = crate::space::dist(old_ball.pos, target_ball.pos)
                                    - f64::from(old_ball.radius) - f64::from(target_ball.radius);
                                assert!(gap > super::super::super::MAX_BOARDING_DISTANCE, "target surface gap must exceed boarding range");
                            }
                            _ => unreachable!(),
                        }
                    }).await.expect("system actor");
                }
                _ => (),
            }
            let refusal = super::super::super::board(&mut c, target, context).await;
            assert!(matches!(&refusal, Err(crate::services::CallError::User { .. })), "{label} must be a normal refusal");
            if label == "missing skill" {
                assert!(matches!(refusal, Err(crate::services::CallError::User { msg, .. }) if msg == crate::messages::MSG_FITTING_HAS_SKILL_PREREQUISITES));
            }
            assert_eq!(fingerprint(&test_db).await, snapshot, "{label}");
            assert_eq!(actor().await, state, "{label}");
            assert_eq!(c.session.int("shipid"), Some(old), "{label}");
            assert_eq!(c.outbox.len(), queued, "{label}");
            assert!(received.try_recv().is_err(), "{label} emitted a destiny notification");
        }
        for id in [old, occupied, warp, cloak, far, unskilled] { crate::services::combat::forget_ship(id); }
    }).await;
    cleanup(&db, foreign_account, foreign_owner).await;
    cleanup(&db, account, owner).await;
    assert_eq!(fingerprint(&db).await, before);
    result.unwrap();
}
