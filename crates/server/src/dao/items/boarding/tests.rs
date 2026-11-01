//! Database boarding transitions, refusals, rollback and competing transactions.
use super::*;
use tokio_postgres::{Client, NoTls, Transaction};

async fn connect() -> Client {
    let cfg = crate::config::tests::test_database();
    let (client, connection) = cfg.to_pg_config().unwrap().connect(NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    client
}

#[derive(Clone, Copy)]
struct Fixture { account: i64, character: i64, other: i64, old: i64, target: i64, spare: i64 }

async fn fixture(tx: &Transaction<'_>) -> Fixture {
    let account = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ('boarding-fixture-' || nextval('game.accounts_user_id_seq')::text,$1,0) RETURNING user_id", &[&&b"fixture"[..]]).await.unwrap().get("user_id");
    let mut chars = [0_i64; 2];
    for character in &mut chars {
        *character = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id,station_id,solar_system_id) VALUES ($1,'boarding-fixture-' || nextval('game.characters_character_id_seq')::text,1,1,1,1,NULL,1) RETURNING character_id", &[&account]).await.unwrap().get("character_id");
    }
    let mut ships = [0_i64; 3];
    for ship in &mut ships {
        *ship = tx.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,x,y,z) VALUES (1,$1,1,0,1,true,0,0,0) RETURNING item_id", &[&chars[0]]).await.unwrap().get("item_id");
    }
    tx.execute("UPDATE game.characters SET ship_id=$2 WHERE character_id=$1", &[&chars[0], &ships[0]]).await.unwrap();
    Fixture { account, character: chars[0], other: chars[1], old: ships[0], target: ships[1], spare: ships[2] }
}

async fn hashes(tx: &Transaction<'_>, f: &Fixture) -> Vec<String> {
    tx.query("SELECT md5(row_to_json(t)::text) AS hash FROM game.characters t WHERE character_id=ANY($1) UNION ALL SELECT md5(row_to_json(t)::text) AS hash FROM game.items t WHERE item_id=ANY($2) ORDER BY hash", &[&&[f.character, f.other][..], &&[f.old, f.target, f.spare][..]]).await.unwrap().iter().map(|r| r.get("hash")).collect()
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn boarding_guards_and_rollback() {
    let mut client = connect().await;
    let mut tx = client.transaction().await.unwrap();
    let f = fixture(&tx).await;
    let before = hashes(&tx, &f).await;
    let save = tx.savepoint("board").await.unwrap();
    assert!(board_in_space(&save, f.character, f.old, f.target, 1, 1, 1, [1.0, 2.0, 3.0], [4.0, 5.0, 6.0]).await.unwrap());
    let row = save.query_one("SELECT ship_id FROM game.characters WHERE character_id=$1", &[&f.character]).await.unwrap();
    assert_eq!(row.get::<_, i64>("ship_id"), f.target);
    let row = save.query_one("SELECT x,y,z FROM game.items WHERE item_id=$1", &[&f.old]).await.unwrap();
    assert_eq!([row.get::<_, f64>("x"), row.get("y"), row.get("z")], [1.0, 2.0, 3.0]);
    save.rollback().await.unwrap();
    assert_eq!(before, hashes(&tx, &f).await);
    for pos in [[f64::NAN, 0.0, 0.0], [0.0, f64::INFINITY, 0.0], [0.0, 0.0, f64::NEG_INFINITY]] {
        assert!(!board_in_space(&tx, f.character, f.old, f.target, 1, 1, 1, pos, [0.0; 3]).await.unwrap());
        assert!(!board_in_space(&tx, f.character, f.old, f.target, 1, 1, 1, [0.0; 3], pos).await.unwrap());
    }
    assert!(!board_in_space(&tx, f.character, f.old, f.old, 1, 1, 1, [0.0; 3], [0.0; 3]).await.unwrap());
    assert!(!board_in_space(&tx, f.character, f.old, f.target, 2, 1, 1, [0.0; 3], [0.0; 3]).await.unwrap());
    assert!(!board_in_space(&tx, f.character, f.old, f.target, 1, 2, 1, [0.0; 3], [0.0; 3]).await.unwrap());
    assert!(!board_in_space(&tx, f.character, f.old, f.target, 1, 1, 2, [0.0; 3], [0.0; 3]).await.unwrap());
    for sql in ["UPDATE game.items SET owner_id=0 WHERE item_id=$1", "UPDATE game.items SET quantity=2 WHERE item_id=$1", "UPDATE game.items SET singleton=false WHERE item_id=$1", "UPDATE game.items SET location_id=2 WHERE item_id=$1", "UPDATE game.items SET flag_id=1 WHERE item_id=$1", "UPDATE game.items SET x='NaN'::float8 WHERE item_id=$1", "UPDATE game.items SET y='Infinity'::float8 WHERE item_id=$1", "UPDATE game.items SET z=NULL WHERE item_id=$1"] {
        for id in [f.old, f.target] {
            let save = tx.savepoint("bad_item").await.unwrap();
            save.execute(sql, &[&id]).await.unwrap();
            assert!(!board_in_space(&save, f.character, f.old, f.target, 1, 1, 1, [0.0; 3], [0.0; 3]).await.unwrap());
            save.rollback().await.unwrap();
        }
    }
    for sql in ["UPDATE game.characters SET deleted=true WHERE character_id=$1", "UPDATE game.characters SET ship_id=NULL WHERE character_id=$1", "UPDATE game.characters SET station_id=1 WHERE character_id=$1", "UPDATE game.characters SET solar_system_id=2 WHERE character_id=$1"] {
        let save = tx.savepoint("bad_character").await.unwrap();
        save.execute(sql, &[&f.character]).await.unwrap();
        assert!(!board_in_space(&save, f.character, f.old, f.target, 1, 1, 1, [0.0; 3], [0.0; 3]).await.unwrap());
        save.rollback().await.unwrap();
    }
    let save = tx.savepoint("occupied").await.unwrap();
    save.execute("UPDATE game.characters SET ship_id=$2,deleted=true WHERE character_id=$1", &[&f.other, &f.target]).await.unwrap();
    assert!(!board_in_space(&save, f.character, f.old, f.target, 1, 1, 1, [0.0; 3], [0.0; 3]).await.unwrap());
    save.rollback().await.unwrap();
    assert_eq!(before, hashes(&tx, &f).await);
    tx.execute("UPDATE game.characters SET station_id=1 WHERE character_id=$1", &[&f.character]).await.unwrap();
    tx.execute("UPDATE game.items SET flag_id=$2 WHERE item_id=ANY($1)", &[&&[f.old, f.target][..], &FLAG_HANGAR]).await.unwrap();
    assert!(!board_in_station(&tx, f.character, f.old, f.target, 2, 1, 1).await.unwrap());
    assert!(board_in_station(&tx, f.character, f.old, f.target, 1, 1, 1).await.unwrap());
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn competing_transactions_have_one_winner() {
    let mut first = connect().await;
    let setup = first.transaction().await.unwrap();
    let f = fixture(&setup).await;
    setup.commit().await.unwrap();
    let result = tokio::spawn(async move {
        let tx = first.transaction().await.unwrap();
        assert!(board_in_space(&tx, f.character, f.old, f.target, 1, 1, 1, [1.0; 3], [2.0; 3]).await.unwrap());
        let character = f.character; let old = f.old; let spare = f.spare;
        let (ready, started) = tokio::sync::oneshot::channel();
        let contender = tokio::spawn(async move {
            let mut client = connect().await;
            let tx = client.transaction().await.unwrap();
            ready.send(()).unwrap();
            let won = board_in_space(&tx, character, old, spare, 1, 1, 1, [3.0; 3], [4.0; 3]).await.unwrap();
            tx.rollback().await.unwrap();
            won
        });
        started.await.unwrap();
        let monitor = connect().await;
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let waiting: bool = monitor.query_one("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE 'SELECT character_id FROM game.characters WHERE character_id=%FOR UPDATE') AS waiting", &[]).await.unwrap().get("waiting");
                if waiting { break; }
                tokio::task::yield_now().await;
            }
        }).await.expect("contender did not wait for the character lock");
        tx.commit().await.unwrap();
        assert!(!contender.await.unwrap());
        let check = first.query_one("SELECT ship_id FROM game.characters WHERE character_id=$1", &[&f.character]).await.unwrap();
        assert_eq!(check.get::<_, i64>("ship_id"), f.target);
    }).await;
    let mut first = connect().await;
    let cleanup = first.transaction().await.unwrap();
    cleanup.execute("DELETE FROM game.items WHERE item_id=ANY($1)", &[&&[f.old, f.target, f.spare][..]]).await.unwrap();
    cleanup.execute("DELETE FROM game.characters WHERE user_id=$1", &[&f.account]).await.unwrap();
    cleanup.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&f.account]).await.unwrap();
    cleanup.commit().await.unwrap();
    result.unwrap();
}
