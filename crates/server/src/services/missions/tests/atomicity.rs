use crate::services::{missions, Call, CallError};
use crate::services::tests::{client, database_message, fixture};
use marshal::Value;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn mission_acceptance_rolls_back_arc_and_offer_when_cargo_overflows() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut c, _peer) = client(test_db.clone(), f.character).await;
        let step = c.sde.missions.steps().find(|step| missions::agent(step.agent).is_some() && c.sde.missions.get(step.mission_id).is_some()).expect("dedicated SDE must contain an arc step with a retail agent");
        let agent = missions::agent(step.agent).unwrap();
        fixture::seed_mission(&test_db, f.character, agent.id, agent.station_id, agent.system_id, step.mission_id, step.arc).await.unwrap();
        let before = fixture::mission_fingerprint(&test_db, f.character).await.unwrap();
        let kwargs = Value::None;
        let bind = Value::Int(agent.id);
        let args = [Value::Int(3)];
        let call = Call { service: "agentMgr", method: "DoAction", args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&bind) };
        match missions::dispatch(&mut c, &call).await.unwrap() {
            Err(CallError::Internal(error)) => assert_eq!(database_message(&error), Some("integer out of range")),
            _ => panic!("courier overflow was not propagated"),
        }
        assert_eq!(fixture::mission_fingerprint(&test_db, f.character).await.unwrap(), before);
        assert!(c.outbox.is_empty());
        fixture::clear_cargo_stack(&test_db, f.character).await.unwrap();
        assert!(missions::dispatch(&mut c, &call).await.unwrap().is_ok());
        assert!(!c.outbox.is_empty());
        let accepted = fixture::mission_fingerprint(&test_db, f.character).await.unwrap();
        c.outbox.clear();
        let _ = missions::dispatch(&mut c, &call).await.unwrap();
        assert_eq!(fixture::mission_fingerprint(&test_db, f.character).await.unwrap(), accepted);
        assert!(c.outbox.is_empty());
    }).await;
    fixture::cleanup(&db, f).await.unwrap();
    assert_eq!(fixture::remaining(&db, f).await.unwrap(), 0);
    assert!(result.is_ok(), "mission rollback task failed: {result:?}");
}

