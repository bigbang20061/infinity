//! Remap validation and persistence regressions.
use super::*;
use super::super::completion;
fn values(values: [i64; 5]) -> Value {
    Value::Dict(CHAR_ATTRIBUTES.into_iter().zip(values).map(|(id, value)| (Value::Int(id), Value::Int(value))).collect())
}
async fn use_real_skill(db: &crate::db::Db, character: i64, sde: &crate::sde::Sde) -> i32 {
    let type_id = sde.types.iter().find(|t| t.category_id == Some(crate::consts::CATEGORY_SKILL) && super::super::requirements::requirements(sde, t.type_id).next().is_none()).expect("SDE must contain a skill without prerequisites").type_id;
    db.execute("UPDATE game.skills SET type_id=$2 WHERE character_id=$1", &[&character, &type_id]).await.unwrap();
    db.execute("UPDATE game.skill_queue SET type_id=$2 WHERE character_id=$1", &[&character, &type_id]).await.unwrap();
    type_id
}

#[test]
fn exact_integer_attributes_and_total_are_required() {
    assert_eq!(parse(&values([17, 17, 17, 21, 27])), Ok([17, 17, 17, 21, 27]));
    assert_eq!(parse(&values([16, 18, 17, 21, 27])), Err(Invalid::Low));
    assert_eq!(parse(&values([28, 17, 17, 20, 17])), Err(Invalid::High));
    assert_eq!(parse(&values([17, 17, 17, 20, 27])), Err(Invalid::Misallocated));
    let mut duplicate = match values([17, 17, 17, 21, 27]) { Value::Dict(pairs) => pairs, _ => unreachable!() };
    duplicate[1].0 = Value::Int(CHAR_ATTRIBUTES[0]);
    assert_eq!(parse(&Value::Dict(duplicate)), Err(Invalid::Malformed));
    let mut unknown = match values([17, 17, 17, 21, 27]) { Value::Dict(pairs) => pairs, _ => unreachable!() };
    unknown[0].0 = Value::Int(999);
    assert_eq!(parse(&Value::Dict(unknown)), Err(Invalid::Malformed));
    let mut float = match values([17, 17, 17, 21, 27]) { Value::Dict(pairs) => pairs, _ => unreachable!() };
    float[0].1 = Value::Float(f64::NAN);
    assert_eq!(parse(&Value::Dict(float)), Err(Invalid::Malformed));
    for (reason, expected) in [(Invalid::Low, crate::messages::MSG_RESPEC_ATTRIBUTES_TOO_LOW), (Invalid::High, crate::messages::MSG_RESPEC_ATTRIBUTES_TOO_HIGH), (Invalid::Misallocated, crate::messages::MSG_RESPEC_ATTRIBUTES_MISALLOCATED)] {
        match invalid_error(reason) { CallError::User { msg, dict } => { assert_eq!(msg, expected); assert!(dict.is_empty()); }, _ => panic!("expected user error") }
    }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn entitlement_persists_and_paused_queue_remains_paused() {
    use crate::{db::Db, services::tests::{client, fixture}};
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    fixture::seed_skill(&db, f.character).await.unwrap();
    db.execute("UPDATE game.skill_queue SET start_time=NULL, end_time=NULL WHERE character_id=$1", &[&f.character]).await.unwrap();
    let (mut c, _peer) = client(db.clone(), f.character).await;
    use_real_skill(&db, f.character, &c.sde).await;
    assert!(apply(&mut c, f.character, &values([17, 17, 17, 20, 27])).await.is_err());
    assert!(attributes::respec(&db, f.character).await.unwrap().is_none());
    get(&mut c, f.character).await.unwrap_or_else(|_| panic!("initialize remap"));
    let initial = attributes::respec(&db, f.character).await.unwrap().unwrap().free_respecs;
    apply(&mut c, f.character, &values([17, 17, 17, 21, 27])).await.unwrap_or_else(|_| panic!("timed remap"));
    let first = attributes::respec(&db, f.character).await.unwrap().unwrap();
    assert_eq!(first.free_respecs, initial);
    assert_eq!(first.bases.as_deref(), Some(&[17, 17, 17, 21, 27][..]));
    let deadline = first.next_timed_respec;
    let effective = super::super::attributes::character_attributes_db(&db, &c.sde, f.character).await.unwrap_or_else(|_| panic!("effective attributes"));
    for (id, base) in CHAR_ATTRIBUTES.into_iter().zip([17, 17, 17, 21, 27]) { assert_eq!(effective[&id], f64::from(base)); }
    let rows = queue::list(&db, f.character).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].start_time.is_none() && rows[0].end_time.is_none());
    if initial > 0 {
        apply(&mut c, f.character, &values([18, 17, 17, 20, 27])).await.unwrap_or_else(|_| panic!("bonus remap"));
        let second = attributes::respec(&db, f.character).await.unwrap().unwrap();
        assert_eq!(second.free_respecs, initial - 1);
        assert_eq!(second.next_timed_respec, deadline);
        get(&mut c, f.character).await.unwrap_or_else(|_| panic!("read persisted remap"));
        assert_eq!(attributes::respec(&db, f.character).await.unwrap().unwrap().free_respecs, initial - 1);
        db.execute("UPDATE game.character_respec SET free_respecs=0 WHERE character_id=$1", &[&f.character]).await.unwrap();
        match apply(&mut c, f.character, &values([17, 17, 17, 21, 27])).await {
            Err(CallError::User { msg, dict }) => {
                assert_eq!(msg, crate::messages::MSG_RESPEC_TOO_SOON);
                assert!(matches!(dict.as_slice(), [(key, Value::Int(value))] if key == "nextTime" && Some(*value) == deadline));
            }
            _ => panic!("expected timed entitlement refusal"),
        }
        let refused = attributes::respec(&db, f.character).await.unwrap().unwrap();
        assert_eq!(refused.next_timed_respec, deadline);
        assert_eq!(refused.bases.as_deref(), Some(&[18, 17, 17, 20, 27][..]));
    }
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn active_queue_banks_progress_and_reschedules() {
    use crate::{db::Db, services::tests::{client, fixture}};
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    fixture::seed_skill(&db, f.character).await.unwrap();
    let now = filetime_now();
    db.execute("UPDATE game.skill_queue SET start_time=$2, end_time=$3 WHERE character_id=$1", &[&f.character, &(now - FILETIME_DAY), &(now + FILETIME_DAY)]).await.unwrap();
    let (mut c, _peer) = client(db.clone(), f.character).await;
    use_real_skill(&db, f.character, &c.sde).await;
    apply(&mut c, f.character, &values([17, 17, 17, 21, 27])).await.unwrap_or_else(|_| panic!("remap active queue"));
    let skill = skill_dao::current(&db, f.character).await.unwrap();
    assert!(skill[0].skill_points >= 50);
    let row = queue::active(&db, f.character).await.unwrap().unwrap();
    assert!(row.start_sp >= 50);
    let id = format!("{}:{}", completion::KIND, f.character);
    let job = db.query_one("SELECT due_at FROM game.jobs WHERE job_id=$1", &[&id]).await.unwrap();
    assert_eq!(Some(job.get::<_, i64>("due_at")), row.end_time);
    crate::scheduler::cancel(&db, &id).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn remap_rolls_back_on_queue_failure_and_batches_overdue_completion() {
    use crate::{db::{Db, outbox::{Effect, Notification}}, lua::events::GameEvent, services::tests::{client, fixture}};
    use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    fixture::seed_skill(&db, f.character).await.unwrap();
    let now = filetime_now();
    db.execute("INSERT INTO game.skill_queue (character_id, position, type_id, to_level, start_sp, dest_sp, start_time, end_time) VALUES ($1, 1, 1, 2, 100, 1415, $2, $3)", &[&f.character, &(now - FILETIME_DAY), &(now + FILETIME_DAY)]).await.unwrap();
    let (c, _peer) = client(db.clone(), f.character).await;
    let type_id = use_real_skill(&db, f.character, &c.sde).await;
    attributes::initialize_respec(&db, f.character, 1).await.unwrap();
    let id = format!("{}:{}", completion::KIND, f.character);
    completion::reschedule(&db, f.character).await.unwrap_or_else(|_| panic!("seed completion job"));
    let snapshot_sql = "SELECT (SELECT row_to_json(r)::text FROM game.character_respec r WHERE character_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(s)::text, '' ORDER BY type_id), '') FROM game.skills s WHERE character_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(q)::text, '' ORDER BY position), '') FROM game.skill_queue q WHERE character_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(h)::text, '' ORDER BY history_id), '') FROM game.skill_history h WHERE character_id=$1) || '|' || (SELECT COALESCE(row_to_json(j)::text, '') FROM game.jobs j WHERE job_id=$2) AS snapshot";
    let before: String = db.query_one(snapshot_sql, &[&f.character, &id]).await.unwrap().get("snapshot");
    let function = format!("atomicity_remap_{}", f.character);
    db.execute(&format!("CREATE FUNCTION game.{function}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.character_id = {} THEN RAISE EXCEPTION 'remap fixture'; END IF; RETURN NEW; END $$", f.character), &[]).await.unwrap();
    db.execute(&format!("CREATE TRIGGER {function} BEFORE INSERT ON game.skill_queue FOR EACH ROW EXECUTE FUNCTION game.{function}()"), &[]).await.unwrap();
    let sde = c.sde.clone();
    let dispatched = Arc::new(AtomicBool::new(false));
    let flag = dispatched.clone();
    let result: Result<Value, CallError> = db.transaction(|tx, outbox: &mut Outbox| {
        let sde = sde.clone();
        Box::pin(async move { apply_tx(tx, outbox, &sde, f.character, &[17, 17, 17, 21, 27], now).await })
    }, |value, _outbox| async move { flag.store(true, Ordering::SeqCst); Ok(value) }).await;
    assert!(matches!(result, Err(CallError::Internal(_))));
    assert!(!dispatched.load(Ordering::SeqCst));
    let after: String = db.query_one(snapshot_sql, &[&f.character, &id]).await.unwrap().get("snapshot");
    assert_eq!(before, after);
    db.execute(&format!("DROP TRIGGER {function} ON game.skill_queue"), &[]).await.unwrap();
    db.execute(&format!("DROP FUNCTION game.{function}()"), &[]).await.unwrap();
    let effects = db.transaction(|tx, outbox: &mut Outbox| {
        let sde = sde.clone();
        Box::pin(async move { apply_tx(tx, outbox, &sde, f.character, &[17, 17, 17, 21, 27], now).await })
    }, |_, outbox| async move { Ok::<_, CallError>(outbox.into_entries()) }).await.unwrap_or_else(|_| panic!("remap transaction"));
    let (mut changed, mut brains, mut trained) = (0, 0, 0);
    for effect in effects {
        match effect {
            Effect::Notification(Notification::Pilot { char_id, event, .. }) if char_id == f.character => {
                if event == "OnServerSkillsChanged" { changed += 1; }
                if event == BRAIN_REFRESH { brains += 1; }
            }
            Effect::Event(GameEvent::SkillTrained { char_id, type_id: trained_type, level: 1 }) if char_id == f.character && trained_type == type_id => trained += 1,
            _ => {}
        }
    }
    assert_eq!((changed, brains, trained), (1, 1, 1));
    assert!(queue::active(&db, f.character).await.unwrap().is_some());
    crate::scheduler::cancel(&db, &id).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn stale_entitlement_write_is_refused() {
    use crate::{db::Db, services::tests::fixture};
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let state = attributes::initialize_respec(&db, f.character, 1).await.unwrap();
    db.execute("UPDATE game.character_respec SET free_respecs=2 WHERE character_id=$1", &[&f.character]).await.unwrap();
    assert!(!attributes::save_respec(&db, f.character, &[17, 17, 17, 21, 27], &state, 0, 1, 2).await.unwrap());
    let current = attributes::respec(&db, f.character).await.unwrap().unwrap();
    assert_eq!(current.free_respecs, 2);
    assert!(current.bases.is_none());
    fixture::cleanup(&db, f).await.unwrap();
}
