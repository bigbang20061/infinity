//! Slash grammar and durable job operator regressions; database tests require a
//! dedicated database and must run serially without a scheduler process.
use super::*;
use crate::scheduler::Job;

#[test]
fn grammar_requires_exact_arguments() {
    assert_eq!(parse(&[]), Some(Command::List));
    assert_eq!(parse(&["retry", "a"]), Some(Command::Retry("a")));
    assert_eq!(parse(&["cancel", "a"]), Some(Command::Cancel("a")));
    for args in [&["list"][..], &["list", "extra"][..], &["retry"][..], &["retry", ""][..], &["cancel"][..], &["cancel", "a", "extra"][..], &["unknown"][..]] {
        assert_eq!(parse(args), None);
    }
}

#[tokio::test]
#[ignore = "requires an empty dedicated INFINITY_TEST_DATABASE_URL; run serially"]
async fn cancelled_periodic_job_is_restored_by_seed() {
    let db = Db::connect(&crate::config::tests::scheduler_database()).await.unwrap();
    assert!(jobs::counts(&db).await.unwrap().is_empty());
    scheduler::seed(&db).await.unwrap();
    assert!(command(&db, &["cancel", scheduler::BILLS]).await.is_ok());
    assert!(!jobs::upcoming(&db).await.unwrap().iter().any(|job| job.id == scheduler::BILLS));
    scheduler::seed(&db).await.unwrap();
    let counts = jobs::counts(&db).await.unwrap();
    assert_eq!(counts.iter().map(|row| row.count).sum::<i64>(), 3);
    let due = jobs::upcoming(&db).await.unwrap();
    for kind in scheduler::PERIODIC_KINDS {
        assert!(due.iter().any(|job| job.id == kind && job.kind == kind));
        assert_eq!(scheduler::cancel(&db, kind).await.unwrap(), 1);
    }
}

#[tokio::test]
#[ignore = "requires an empty dedicated INFINITY_TEST_DATABASE_URL; run serially"]
async fn operator_stats_retry_and_cancel() {
    let db = Db::connect(&crate::config::tests::scheduler_database()).await.unwrap();
    assert!(jobs::next_due(&db, &[]).await.unwrap().is_none());
    let now = filetime_now();
    let id = format!("jobs-command-test:{now}");
    let job = Job { id: id.clone(), kind: "jobs-command-test".into(), due_at: now, payload: "{\"version\":1}".into() };
    scheduler::schedule(&db, &job).await.unwrap();
    let counts = jobs::counts(&db).await.unwrap();
    assert!(counts.iter().any(|c| c.kind == job.kind && c.state == "scheduled" && c.count >= 1));
    assert!(jobs::upcoming(&db).await.unwrap().iter().any(|j| j.id == id));
    let claim = jobs::claim(&db, now, now + 1, 1, &[]).await.unwrap().pop().unwrap();
    assert_eq!(claim.job.id, id);
    assert!(retry(&db, &id).await.unwrap());
    assert_eq!(jobs::ack(&db, &id, claim.token).await.unwrap(), 0);
    let row = jobs::tests::inspect(&db, &id).await.unwrap();
    assert_eq!(row.attempt, 1);
    let claim = jobs::claim(&db, filetime_now(), i64::MAX, 1, &[]).await.unwrap().pop().unwrap();
    assert_eq!(claim.job.payload, job.payload);
    assert_eq!(jobs::fail(&db, &id, claim.token, now, now, "failure", true).await.unwrap(), 1);
    assert!(retry(&db, &id).await.unwrap());
    let row = jobs::tests::inspect(&db, &id).await.unwrap();
    assert_eq!(row.attempt, 0);
    assert_eq!(row.last_error, None);
    let running = jobs::claim(&db, filetime_now(), i64::MAX, 1, &[]).await.unwrap().pop().unwrap();
    assert!(command(&db, &["cancel", &id]).await.is_ok());
    assert_eq!(jobs::ack(&db, &id, running.token).await.unwrap(), 0);
    assert!(!retry(&db, &id).await.unwrap());
    assert!(command(&db, &["cancel", &id]).await.is_err());
}
