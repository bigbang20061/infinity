//! Durable queue completion and replay regressions.
use super::*;
use crate::services::tests::{client, fixture};

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn completion_emits_one_lua_event_per_new_level_and_none_on_replay() {
    use crate::db::outbox::Effect;
    use crate::lua::events::GameEvent;
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    fixture::seed_skill(&db, f.character).await.unwrap();
    db.execute("INSERT INTO game.skill_queue (character_id,position,type_id,to_level,start_sp,dest_sp,start_time,end_time) VALUES ($1,1,1,2,100,200,1,2)", &[&f.character]).await.unwrap();
    let events = db.transaction(|tx, outbox: &mut Outbox| Box::pin(async move {
        crate::dao::characters::selection::lock_character(tx, f.character).await.map_err(db_err)?;
        let first = complete_tx(tx, outbox, f.character, filetime_now()).await?;
        let replay = complete_tx(tx, outbox, f.character, filetime_now()).await?;
        assert_eq!(first, vec![(1, 1), (1, 2)]);
        assert!(replay.is_empty());
        Ok(())
    }), |(), outbox| async move { Ok::<_, CallError>(outbox.into_entries()) }).await.unwrap_or_else(|_| panic!("failed to complete and replay skill queue"));
    let levels: Vec<_> = events.into_iter().filter_map(|entry| match entry {
        Effect::Event(GameEvent::SkillTrained { char_id, type_id, level }) if char_id == f.character && type_id == 1 => Some(level),
        _ => None,
    }).collect();
    assert_eq!(levels, vec![1, 2]);
    let history = crate::dao::skills::history::list(&db, f.character, 50).await.unwrap();
    assert_eq!(history.len(), 2);
    assert!(history.iter().all(|row| row.event_type_id == super::super::history::TRAINING_COMPLETE));
    assert_eq!(history.iter().map(|row| row.absolute_points).collect::<Vec<_>>(), vec![200, 100]);
    fixture::cleanup(&db, f).await.unwrap();
}
#[tokio::test]

#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn restart_catches_up_adjacent_levels_and_reschedules_then_cancels() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    fixture::seed_skill(&db, f.character).await.unwrap();
    let future = filetime_now() + crate::consts::FILETIME_MINUTE;
    db.execute("INSERT INTO game.skill_queue (character_id,position,type_id,to_level,start_sp,dest_sp,start_time,end_time) VALUES ($1,1,1,2,100,200,1,2),($1,2,1,3,200,300,2,$2)", &[&f.character, &future]).await.unwrap();
    let (c, _peer) = client(db.clone(), f.character).await;
    seed(&db).await.unwrap_or_else(|_| panic!("failed to seed skill completion jobs after restart"));
    let id = format!("{KIND}:{}", f.character);
    let job = Job { id: id.clone(), kind: KIND.into(), due_at: 1, payload: f.character.to_string() };
    handle(db.clone(), c.world.clone(), job.clone()).await.unwrap();
    let skills = training::current(&db, f.character).await.unwrap();
    assert_eq!((skills[0].level, skills[0].skill_points), (2, 200));
    let rows = queue::list(&db, f.character).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].position, rows[0].to_level), (0, 3));
    let scheduled = db.query_one("SELECT due_at FROM game.jobs WHERE job_id = $1", &[&id]).await.unwrap();
    assert_eq!(scheduled.get::<_, i64>("due_at"), future);
    let before = fixture::skill_fingerprint(&db, f.character).await.unwrap();
    handle(db.clone(), c.world.clone(), job.clone()).await.unwrap();
    assert_eq!(fixture::skill_fingerprint(&db, f.character).await.unwrap(), before);
    db.execute("UPDATE game.skill_queue SET end_time = 1 WHERE character_id = $1", &[&f.character]).await.unwrap();
    handle(db.clone(), c.world.clone(), job).await.unwrap();
    assert!(queue::list(&db, f.character).await.unwrap().is_empty());
    assert!(db.query_opt("SELECT job_id FROM game.jobs WHERE job_id = $1", &[&id]).await.unwrap().is_none());
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires an empty dedicated INFINITY_TEST_DATABASE_URL with SDE and serial execution"]
async fn real_runner_delivers_due_completion_to_online_pilot_channel() {
    use std::time::Duration;
    use tokio::sync::{mpsc, watch};

    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    assert!(crate::dao::jobs::next_due(&db, &[]).await.unwrap().is_none());
    let f = fixture::create(&db).await.unwrap();
    fixture::seed_skill(&db, f.character).await.unwrap();
    let (c, _peer) = client(db.clone(), f.character).await;
    let (sender, mut receiver) = mpsc::unbounded_channel();
    c.world.register_pilot(f.character, sender);
    let due = filetime_now() + crate::client::filetime_ticks_from_seconds(5);
    db.execute("UPDATE game.skill_queue SET start_time = $2, end_time = $3 WHERE character_id = $1", &[&f.character, &filetime_now(), &due]).await.unwrap();
    seed(&db).await.unwrap_or_else(|_| panic!("failed to seed skill completion job for runner"));
    let id = format!("{KIND}:{}", f.character);
    let (shutdown, watch) = watch::channel(false);
    let mut runner = scheduler::runner::start(
        db.clone(), c.world.clone(), vec![scheduler::register(KIND, handle)],
        crate::config::scheduler::SchedulerConfig::default(), watch,
    ).unwrap();
    scheduler::wake();

    // A separate task lets the runner drain and fixture cleanup run after assertion failures.
    let observed = tokio::spawn(async move {
        assert!(filetime_now() + crate::client::filetime_ticks_from_seconds(1) < due);
        assert!(tokio::time::timeout(Duration::from_secs(1), receiver.recv()).await.is_err(),
            "pilot received a notification before the deadline");
        let mut events = Vec::new();
        tokio::time::timeout(Duration::from_secs(15), async {
            while events.len() < 3 {
                let (event, args) = receiver.recv().await.expect("pilot channel closed");
                assert!(filetime_now() >= due, "notification arrived before the deadline");
                if event == "OnServerSkillsChanged" {
                    assert!(matches!(args.get(2), Some(Value::Int(now)) if *now >= due));
                }
                events.push(event);
            }
        }).await.expect("runner did not deliver completion notifications");
        (events, receiver)
    }).await;
    let _ = shutdown.send(true);
    let drained = tokio::time::timeout(Duration::from_secs(10), &mut runner).await;
    if drained.is_err() {
        runner.abort();
        let _ = runner.await;
    }
    let checked = tokio::spawn({
        let db = db.clone();
        let id = id.clone();
        async move {
            drained.expect("runner did not drain").expect("runner panicked");
            let (mut events, mut receiver) = observed.expect("notification assertions failed");
            while let Ok((event, _)) = receiver.try_recv() { events.push(event); }
            assert_eq!(events.iter().filter(|event| event.as_str() == "OnServerSkillsChanged").count(), 1);
            assert_eq!(events.iter().filter(|event| event.as_str() == BRAIN_REFRESH).count(), 1);
            assert_eq!(events.iter().filter(|event| event.as_str() == "OnNewSkillQueueSaved").count(), 1);
            let skills = training::current(&db, f.character).await.unwrap();
            assert_eq!(skills.len(), 1);
            assert_eq!((skills[0].level, skills[0].skill_points), (1, 100));
            assert!(queue::list(&db, f.character).await.unwrap().is_empty());
            assert!(db.query_opt("SELECT job_id FROM game.jobs WHERE job_id = $1", &[&id]).await.unwrap().is_none());
        }
    }).await;
    scheduler::cancel(&db, &id).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
    checked.expect("runner completion assertions failed");
}
