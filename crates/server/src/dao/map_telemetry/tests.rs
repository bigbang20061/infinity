use super::*;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn receipts_windows_visits_and_retention_are_atomic() {
    crate::tests::init_config();
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let mut tx = client.transaction().await.unwrap();
    let id = next_occurrence(&tx).await.unwrap();
    let other_id = next_occurrence(&tx).await.unwrap();
    assert_ne!(id, other_id);
    // Unique synthetic systems and characters prevent interference with existing data.
    let system = -id;
    let character = -id;
    let other_character = -other_id;
    let time = i64::MIN + 1000;

    {
        let save = tx.savepoint("arrival_rollback").await.unwrap();
        assert!(append(&save, EventSource::JumpOccurrence, id, system, time, EventKind::Jump).await.unwrap());
        record_visit(&save, character, system, time).await.unwrap();
        save.rollback().await.unwrap();
    }
    assert!(visits(&tx, character).await.unwrap().is_empty());
    assert!(append(&tx, EventSource::JumpOccurrence, id, system, time, EventKind::Jump).await.unwrap());
    record_visit(&tx, character, system, time).await.unwrap();
    if append(&tx, EventSource::JumpOccurrence, id, system, time + 1, EventKind::Jump).await.unwrap() {
        record_visit(&tx, character, system, time + 1).await.unwrap();
    }
    assert!(append(&tx, EventSource::JumpOccurrence, other_id, system, time - 1, EventKind::Jump).await.unwrap());
    record_visit(&tx, character, system, time - 1).await.unwrap();
    assert!(append(&tx, EventSource::Killmail, id, system, time, EventKind::Ship).await.unwrap());
    assert!(!append(&tx, EventSource::Killmail, id, system, time, EventKind::Pod).await.unwrap());
    assert!(append(&tx, EventSource::NpcOccurrence, id, system, time + 1, EventKind::Npc).await.unwrap());
    assert!(append(&tx, EventSource::UnpilotedShip, id, system, time + 2, EventKind::Ship).await.unwrap());
    assert!(append(&tx, EventSource::Killmail, other_id, system, time + 2, EventKind::Pod).await.unwrap());
    assert!(append(&tx, EventSource::JumpOccurrence, next_occurrence(&tx).await.unwrap(), system, time + 3, EventKind::Jump).await.unwrap());
    record_visit(&tx, other_character, system, time + 3).await.unwrap();

    let jumps = history(&tx, HistoryStat::Jumps, time, time + 2).await.unwrap();
    assert!(jumps.contains(&History { solar_system_id: system, value1: 1, value2: 0, value3: 0 }));
    let kills = history(&tx, HistoryStat::Kills, time, time + 2).await.unwrap();
    assert!(kills.contains(&History { solar_system_id: system, value1: 3, value2: 1, value3: 1 }));
    let bounded = history(&tx, HistoryStat::Kills, time + 1, time + 1).await.unwrap();
    assert!(bounded.contains(&History { solar_system_id: system, value1: 1, value2: 1, value3: 0 }));
    assert_eq!(visits(&tx, character).await.unwrap(), vec![Visit { solar_system_id: system, last_date_time: time, visits: 2 }]);
    assert_eq!(visits(&tx, other_character).await.unwrap(), vec![Visit { solar_system_id: system, last_date_time: time + 3, visits: 1 }]);

    // Deletion is transactionally rolled back and does not remove cumulative visits.
    assert!(delete_before(&tx, time).await.unwrap() >= 1);
    assert_eq!(visits(&tx, character).await.unwrap()[0].visits, 2);
    assert!(history(&tx, HistoryStat::Jumps, time - 1, time - 1).await.unwrap().iter().all(|row| row.solar_system_id != system));
    assert!(history(&tx, HistoryStat::Jumps, time, time).await.unwrap().iter().any(|row| row.solar_system_id == system));
    tx.rollback().await.unwrap();
    let (mut reconnected, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let check = reconnected.transaction().await.unwrap();
    assert!(visits(&check, character).await.unwrap().is_empty());
    assert!(history(&check, HistoryStat::Jumps, time - 1, time + 3).await.unwrap().iter().all(|row| row.solar_system_id != system));
    check.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn committed_visits_survive_reconnection() {
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let id = next_occurrence(&tx).await.unwrap();
    let character = -id;
    let system = -id;
    assert!(append(&tx, EventSource::JumpOccurrence, id, system, 42, EventKind::Jump).await.unwrap());
    record_visit(&tx, character, system, 42).await.unwrap();
    tx.commit().await.unwrap();
    drop(client);
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    assert_eq!(visits(&tx, character).await.unwrap(), vec![Visit { last_date_time: 42, solar_system_id: system, visits: 1 }]);
    tx.execute("DELETE FROM game.map_events WHERE source = $1 AND source_id = $2", &[&(EventSource::JumpOccurrence as i16), &id]).await.unwrap();
    tx.execute("DELETE FROM game.map_visits WHERE character_id = $1", &[&character]).await.unwrap();
    tx.commit().await.unwrap();
}
