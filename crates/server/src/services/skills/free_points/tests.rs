//! Real free-point settlement, queue retiming and failure coverage.
use super::*;
use crate::services::tests::{client, fixture};
use crate::db::outbox::{Effect, Notification};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn free_points_debit_retimes_queue_and_rolls_back_on_skill_write_failure() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (c, _peer) = client(db.clone(), f.character).await;
    let sde = c.sde.clone();
    let type_id = sde.types.iter().find(|t| t.category_id == Some(crate::consts::CATEGORY_SKILL) && t.published == Some(true) && super::super::requirements::requirements(&sde, t.type_id).next().is_none()).expect("skill without prerequisites").type_id;
    let points = super::super::formulas::sp_for_level(training::skill_type(&sde, type_id).rank, 1);
    let now = filetime_now();
    db.execute("UPDATE game.characters SET free_skill_points=$2 WHERE character_id=$1", &[&f.character, &(points * 3)]).await.unwrap();
    db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,0,0)", &[&f.character, &type_id]).await.unwrap();
    db.execute("INSERT INTO game.skill_queue (character_id,position,type_id,to_level,start_sp,dest_sp,start_time,end_time) VALUES ($1,0,$2,1,0,$3,NULL,NULL),($1,1,$2,2,$3,$4,NULL,NULL)", &[&f.character, &type_id, &points, &super::super::formulas::sp_for_level(training::skill_type(&sde, type_id).rank, 2)]).await.unwrap();
    let before = fixture::skill_fingerprint(&db, f.character).await.unwrap();
    fixture::install_skill_failure(&db, f.character).await.unwrap();
    let dispatched = Arc::new(AtomicBool::new(false));
    let flag = dispatched.clone();
    let failed: Result<(), CallError> = db.transaction(|tx, outbox: &mut crate::db::outbox::Outbox| {
        let sde = sde.clone(); Box::pin(async move { apply_tx(tx, outbox, &sde, f.character, type_id, points, now).await.map(|_| ()) })
    }, |(), _| async move { flag.store(true, Ordering::SeqCst); Ok(()) }).await;
    assert!(failed.is_err());
    assert!(!dispatched.load(Ordering::SeqCst));
    assert_eq!(attributes::free_points(&db, f.character).await.unwrap(), points * 3);
    assert_eq!(fixture::skill_fingerprint(&db, f.character).await.unwrap(), before);
    assert!(history::list(&db, f.character, 50).await.unwrap().is_empty());
    fixture::remove_skill_failure(&db, f.character).await.unwrap();
    let effects = db.transaction(|tx, outbox: &mut crate::db::outbox::Outbox| {
        let sde = sde.clone(); Box::pin(async move { apply_tx(tx, outbox, &sde, f.character, type_id, points, now).await.map(|_| ()) })
    }, |(), outbox| async move { Ok::<_, CallError>(outbox.into_entries()) }).await.unwrap_or_else(|_| panic!("free point settlement failed"));
    assert_eq!(attributes::free_points(&db, f.character).await.unwrap(), points * 2);
    let skill = skill_dao::current(&db, f.character).await.unwrap().pop().unwrap();
    assert_eq!((skill.level, skill.skill_points), (1, points));
    let rows = queue::list(&db, f.character).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].to_level, 2);
    assert_eq!(history::list(&db, f.character, 50).await.unwrap().len(), 1);
    assert!(effects.iter().any(|e| matches!(e, Effect::Notification(Notification::DirectClient { event, .. }) if event == "OnFreeSkillPointsChanged")));
    assert!(!effects.iter().any(|e| matches!(e, Effect::Event(crate::lua::events::GameEvent::SkillTrained { .. }))));
    for amount in [0, -1, points * 3, i64::MAX] {
        let refused: Result<(), CallError> = db.transaction(|tx, outbox: &mut crate::db::outbox::Outbox| {
            let sde = sde.clone(); Box::pin(async move { apply_tx(tx, outbox, &sde, f.character, type_id, amount, now).await.map(|_| ()) })
        }, |(), _| async move { Ok(()) }).await;
        assert!(refused.is_err());
    }
    assert_eq!(attributes::free_points(&db, f.character).await.unwrap(), points * 2);
    assert_eq!(history::list(&db, f.character, 50).await.unwrap().len(), 1);
    db.execute("DELETE FROM game.skill_history WHERE character_id=$1", &[&f.character]).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn overdue_completion_is_merged_with_free_points() {
    use crate::lua::events::GameEvent;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (c, _peer) = client(db.clone(), f.character).await;
    let sde = c.sde.clone();
    let types: Vec<_> = sde.types.iter().filter(|t| t.category_id == Some(crate::consts::CATEGORY_SKILL) && t.published == Some(true) && super::super::requirements::requirements(&sde, t.type_id).next().is_none()).take(2).map(|t| t.type_id).collect();
    assert_eq!(types.len(), 2);
    let (trained, target) = (types[0], types[1]);
    let trained_sp = super::super::formulas::sp_for_level(training::skill_type(&sde, trained).rank, 1);
    let points = super::super::formulas::sp_for_level(training::skill_type(&sde, target).rank, 1);
    let now = filetime_now();
    db.execute("UPDATE game.characters SET free_skill_points=$2 WHERE character_id=$1", &[&f.character, &(points * 2)]).await.unwrap();
    db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,0,0),($1,$3,0,0)", &[&f.character, &trained, &target]).await.unwrap();
    db.execute("INSERT INTO game.skill_queue (character_id,position,type_id,to_level,start_sp,dest_sp,start_time,end_time) VALUES ($1,0,$2,1,0,$3,1,2)", &[&f.character, &trained, &trained_sp]).await.unwrap();
    let effects = db.transaction(|tx, outbox: &mut Outbox| {
        let sde = sde.clone(); Box::pin(async move { apply_tx(tx, outbox, &sde, f.character, target, points, now).await.map(|_| ()) })
    }, |(), outbox| async move { Ok::<_, CallError>(outbox.into_entries()) }).await.unwrap_or_else(|_| panic!("free points settlement failed"));
    let batches: Vec<_> = effects.iter().filter_map(|e| match e { Effect::Notification(Notification::Pilot { char_id, event, args }) if *char_id == f.character && event == "OnServerSkillsChanged" => Some(args), _ => None }).collect();
    assert_eq!(batches.len(), 1);
    let Value::Dict(infos) = &batches[0][0] else { panic!("skills dictionary") };
    assert_eq!(infos.len(), 2);
    for (id, sp) in [(trained, trained_sp), (target, points)] {
        assert!(infos.iter().any(|(key, value)| *key == Value::Int(i64::from(id)) && *value == training::skill_info(&sde, id, 1, sp, now)));
    }
    assert_eq!(effects.iter().filter(|e| matches!(e, Effect::Notification(Notification::Pilot { char_id, event, .. }) if *char_id == f.character && event == crate::client::BRAIN_REFRESH)).count(), 1);
    let events: Vec<_> = effects.iter().filter_map(|e| match e { Effect::Event(GameEvent::SkillTrained { char_id, type_id, level }) => Some((*char_id, *type_id, *level)), _ => None }).collect();
    assert_eq!(events, vec![(f.character, trained, 1)]);
    assert_eq!(attributes::free_points(&db, f.character).await.unwrap(), points);
    assert!(queue::list(&db, f.character).await.unwrap().is_empty());
    let hist = history::list(&db, f.character, 50).await.unwrap();
    assert_eq!(hist.len(), 2);
    assert!(hist.iter().any(|h| h.event_type_id == super::super::history::TRAINING_COMPLETE && h.absolute_points == trained_sp));
    assert!(hist.iter().any(|h| h.event_type_id == super::super::history::FREE_POINTS_USED && h.absolute_points == points));
    crate::scheduler::cancel(&db, &format!("{}:{}", super::super::completion::KIND, f.character)).await.unwrap();
    db.execute("DELETE FROM game.skill_history WHERE character_id=$1", &[&f.character]).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
}
