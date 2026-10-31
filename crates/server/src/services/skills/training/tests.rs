//! Completion event ordering and duplicate suppression for skill training.
use super::*;

mod atomicity;

#[test]
fn completed_levels_are_sorted_and_only_include_new_levels() {
    let done = [(2, 2, 20), (1, 2, 20), (1, 1, 10), (2, 2, 20), (2, 1, 10), (1, 3, 30)];
    let levels = HashMap::from([(1, 1), (2, 0)]);
    assert_eq!(completed_levels(&done, levels), vec![(1, 2), (1, 3), (2, 1), (2, 2)]);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn queue_replacement_and_abort_bank_progress_and_update_deadline() {
    use crate::services::tests::{client, fixture};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    fixture::seed_skill(&db, f.character).await.unwrap();
    let now = filetime_now();
    db.execute("UPDATE game.skill_queue SET start_time=$2, end_time=$3 WHERE character_id=$1", &[&f.character, &(now - crate::consts::FILETIME_MINUTE), &(now + crate::consts::FILETIME_MINUTE)]).await.unwrap();
    let (mut c, _peer) = client(db.clone(), f.character).await;
    let replacement = Value::Dict(vec![(Value::Int(0), Value::tuple([Value::Int(1), Value::Int(1)]))]);
    save_queue(&mut c, f.character, &replacement, true, true).await.unwrap_or_else(|_| panic!("failed to replace skill queue"));
    let skills = training_dao::current(&db, f.character).await.unwrap();
    assert!(skills[0].skill_points >= 50);
    let id = format!("{}:{}", super::super::completion::KIND, f.character);
    let row = db.query_one("SELECT due_at FROM game.jobs WHERE job_id=$1", &[&id]).await.unwrap();
    let queued = queue_dao::active(&db, f.character).await.unwrap().unwrap();
    assert_eq!(Some(row.get::<_, i64>("due_at")), queued.end_time);
    save_queue(&mut c, f.character, &Value::Dict(vec![]), false, false).await.unwrap_or_else(|_| panic!("failed to abort skill queue"));
    assert!(queue_dao::list(&db, f.character).await.unwrap().is_empty());
    assert!(db.query_opt("SELECT job_id FROM game.jobs WHERE job_id=$1", &[&id]).await.unwrap().is_none());
    assert!(training_dao::current(&db, f.character).await.unwrap()[0].skill_points >= 50);
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn queue_save_completion_notifies_once_and_rollback_dispatches_nothing() {
    use crate::db::outbox::{Effect, Notification};
    use crate::lua::events::GameEvent;
    use crate::services::tests::{client, fixture};
    use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    for notify in [true, false] {
        let f = fixture::create(&db).await.unwrap();
        fixture::seed_skill(&db, f.character).await.unwrap();
        let before = fixture::skill_fingerprint(&db, f.character).await.unwrap();
        let (c, _peer) = client(db.clone(), f.character).await;
        let sde = c.sde.clone();
        let dispatched = Arc::new(AtomicBool::new(false));
        let flag = dispatched.clone();
        let result: Result<(), CallError> = db.transaction(|tx, outbox: &mut Outbox| {
            let sde = sde.clone();
            Box::pin(async move {
                save_queue_tx(tx, outbox, &sde, f.character, vec![], false, notify, filetime_now()).await?;
                Err(CallError::Internal(anyhow::anyhow!("force rollback")))
            })
        }, |(), _outbox| async move {
            flag.store(true, Ordering::SeqCst);
            Ok(())
        }).await;
        assert!(result.is_err());
        assert!(!dispatched.load(Ordering::SeqCst));
        assert_eq!(fixture::skill_fingerprint(&db, f.character).await.unwrap(), before);

        let effects = db.transaction(|tx, outbox: &mut Outbox| {
            let sde = sde.clone();
            Box::pin(async move {
                save_queue_tx(tx, outbox, &sde, f.character, vec![], false, notify, filetime_now()).await?;
                Ok(())
            })
        }, |(), outbox| async move { Ok::<_, CallError>(outbox.into_entries()) }).await.unwrap_or_else(|_| panic!("failed to save skill queue and collect effects"));
        let mut changed = 0;
        let mut trained = 0;
        let mut brains = 0;
        let mut queues = 0;
        for effect in effects {
            match effect {
                Effect::Notification(Notification::Pilot { char_id, event, .. }) if char_id == f.character => {
                    if event == "OnServerSkillsChanged" { changed += 1; }
                    if event == crate::client::BRAIN_REFRESH { brains += 1; }
                }
                Effect::Notification(Notification::DirectClient { event, .. }) if event == "OnNewSkillQueueSaved" => queues += 1,
                Effect::Event(GameEvent::SkillTrained { char_id, type_id: 1, level: 1 }) if char_id == f.character => trained += 1,
                _ => {}
            }
        }
        assert_eq!((changed, trained, brains, queues), (1, 1, 1, usize::from(notify)));
        fixture::cleanup(&db, f).await.unwrap();
    }
}
