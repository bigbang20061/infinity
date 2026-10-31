use crate::services::skills::completion::{self, KIND};
use crate::services::tests::{client, fixture};
use crate::services::{skills, Call};
use marshal::Value;
use crate::scheduler::Job;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn scheduled_completion_rolls_back_finished_queue_when_skill_write_fails() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        fixture::seed_skill(&test_db, f.character).await.unwrap();
        let before = fixture::skill_fingerprint(&test_db, f.character).await.unwrap();
        fixture::install_skill_failure(&test_db, f.character).await.unwrap();
        let (mut c, _peer) = client(test_db.clone(), f.character).await;
        for method in ["GetSkills", "GetSkillQueue"] {
            let kwargs = Value::None;
            let call = Call { service: "skillHandler", method, args: &[], kwargs: &kwargs, bound: true, bind_params: None };
            assert!(skills::dispatch(&mut c, &call).await.unwrap().is_ok());
            assert_eq!(fixture::skill_fingerprint(&test_db, f.character).await.unwrap(), before);
        }
        let job = Job { id: format!("{KIND}:{}", f.character), kind: KIND.into(), due_at: 1, payload: f.character.to_string() };
        assert!(completion::handle(test_db.clone(), c.world.clone(), job).await.is_err());
        assert_eq!(fixture::skill_fingerprint(&test_db, f.character).await.unwrap(), before);
        assert!(c.outbox.is_empty());
    }).await;
    fixture::remove_skill_failure(&db, f.character).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
    assert_eq!(fixture::remaining(&db, f).await.unwrap(), 0);
    assert!(result.is_ok(), "skill rollback task failed: {result:?}");
}
