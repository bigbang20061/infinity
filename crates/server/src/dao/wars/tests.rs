//! Transactional mutual-war guards and invite generations on a dedicated database.
use super::*;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn lifecycle_generation_guards_and_rollback() {
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let mut tx = client.transaction().await.unwrap();
    let save = tx.savepoint("wars_fixture").await.unwrap();
    let a: i64 = save.query_one("SELECT nextval('game.wars_war_id_seq') AS id", &[]).await.unwrap().get("id");
    let b = -a;
    lock_owners(&save, &[b, a, a]).await.unwrap();
    assert!(!invites_blocked(&save, a).await.unwrap());
    assert!(set_invites_blocked(&save, a, true).await.unwrap());
    assert!(!set_invites_blocked(&save, a, true).await.unwrap());
    assert!(set_invites_blocked(&save, a, false).await.unwrap());
    let first = Invite { from_owner: a, to_owner: b, sent_at: 1, expires_at: 3 };
    assert!(insert_invite(&save, &first).await.unwrap());
    assert!(!insert_invite(&save, &first).await.unwrap());
    let second = Invite { sent_at: 3, expires_at: 5, ..first.clone() };
    assert!(insert_invite(&save, &second).await.unwrap());
    assert!(!remove_invite(&save, a, b, 1).await.unwrap());
    assert!(!expire_invite(&save, a, b, 1, 3, 5).await.unwrap());
    assert!(!expire_invite(&save, a, b, 3, 5, 4).await.unwrap());
    assert!(expire_invite(&save, a, b, 3, 5, 5).await.unwrap());
    let war = create(&save, a, b, 1, 3).await.unwrap().unwrap();
    assert!(create(&save, b, a, 1, 3).await.unwrap().is_none());
    assert!(activate(&save, war.war_id, 3, 2).await.unwrap().is_none());
    assert!(retract(&save, war.war_id, b, 2, 5).await.unwrap().is_some());
    assert!(activate(&save, war.war_id, 3, 3).await.unwrap().is_none());
    assert!(active_by_owners(&save, &[a,b], 3).await.unwrap().is_empty());
    assert_eq!(active_by_owners(&save, &[a,b], 4).await.unwrap().len(), 1);
    assert!(active_by_owners(&save, &[a,b], 5).await.unwrap().is_empty());
    assert!(finish(&save, war.war_id, 4, 5).await.unwrap().is_none());
    assert!(ongoing_pair(&save, a, b, 5).await.unwrap().is_none());
    assert_eq!(settle_due_pair(&save, a, b, 5).await.unwrap().len(), 1);
    assert!(finish(&save, war.war_id, 5, 5).await.unwrap().is_none());
    assert!(create(&save, b, a, 5, 7).await.unwrap().is_some());
    save.rollback().await.unwrap();
    assert!(get(&tx, war.war_id).await.unwrap().is_none());
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn reversed_owner_locks_serialize_first_creation() {
    let cfg = crate::config::tests::test_database().to_pg_config().unwrap();
    let (mut first, connection) = cfg.connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let (mut second, connection) = cfg.connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let a: i64 = first.query_one("SELECT nextval('game.wars_war_id_seq') AS id", &[]).await.unwrap().get("id");
    let b = -a;
    let tx = first.transaction().await.unwrap();
    let count: i64 = tx.query_one("SELECT count(*) AS count FROM game.war_invite_settings WHERE owner_id=ANY($1)", &[&[a, b].as_slice()]).await.unwrap().get("count");
    assert_eq!(count, 0);
    lock_owners(&tx, &[a, b]).await.unwrap();
    let war = create(&tx, a, b, 1, 2).await.unwrap().unwrap();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let contender = tokio::spawn(async move {
        let tx = second.transaction().await.unwrap();
        let pid: i32 = tx.query_one("SELECT pg_backend_pid() AS pid", &[]).await.unwrap().get("pid");
        ready_tx.send(pid).unwrap();
        lock_owners(&tx, &[b, a]).await.unwrap();
        assert!(create(&tx, b, a, 1, 2).await.unwrap().is_none());
        tx.commit().await.unwrap();
    });
    let pid = ready_rx.await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let waiting: bool = tx.query_one("SELECT pg_backend_pid()=ANY(pg_blocking_pids($1)) AS waiting", &[&pid]).await.unwrap().get("waiting");
            if waiting { break; }
            tokio::task::yield_now().await;
        }
    }).await.expect("contender did not wait for owner locks");
    tx.commit().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), contender).await.unwrap().unwrap();
    let rows = first.query("SELECT owner_id, blocked FROM game.war_invite_settings WHERE owner_id=ANY($1) ORDER BY owner_id", &[&[a, b].as_slice()]).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| !row.get::<_, bool>("blocked")));
    let count: i64 = first.query_one("SELECT count(*) AS count FROM game.wars WHERE LEAST(declared_by,against)=LEAST($1::bigint,$2::bigint) AND GREATEST(declared_by,against)=GREATEST($1::bigint,$2::bigint)", &[&a, &b]).await.unwrap().get("count");
    assert_eq!(count, 1);
    first.execute("DELETE FROM game.wars WHERE war_id=$1", &[&war.war_id]).await.unwrap();
    first.execute("DELETE FROM game.war_invite_settings WHERE owner_id=ANY($1)", &[&[a, b].as_slice()]).await.unwrap();
}
