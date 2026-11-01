use super::*;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn nonfinite_positions_are_neither_loaded_nor_claimed() {
    let db = crate::db::Db::connect(&crate::config::deployment::DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let account: i64 = db.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ('position-fixture-' || nextval('game.accounts_user_id_seq')::text,$1,0) RETURNING user_id", &[&&b"fixture"[..]]).await.unwrap().get("user_id");
    let owner: i64 = db.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,'position-fixture-' || nextval('game.characters_character_id_seq')::text,1,1,1,1) RETURNING character_id", &[&account]).await.unwrap().get("character_id");
    let system = 1_i64;
    let kind = 1_i32;
    let id = crate::dao::items::creation::positioned(&db, kind, owner, system, [0.0; 3]).await.unwrap();
    let result = tokio::spawn({ let db = db.clone(); async move {
        for axis in 0..3 {
            for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
                let mut pos = [0.0; 3];
                pos[axis] = value;
                db.execute("UPDATE game.items SET x=$2,y=$3,z=$4 WHERE item_id=$1", &[&id, &pos[0], &pos[1], &pos[2]]).await.unwrap();
                assert!(!unpiloted(&db, system).await.unwrap().iter().any(|ship| ship.item_id == id));
                assert!(!claim_unpiloted(&db, id, kind, owner, system).await.unwrap());
            }
        }
        db.execute("UPDATE game.items SET x=$2,y=$3,z=$4 WHERE item_id=$1", &[&id, &f64::MAX, &(-f64::MAX), &0.0_f64]).await.unwrap();
        assert!(unpiloted(&db, system).await.unwrap().iter().any(|ship| ship.item_id == id));
        assert!(claim_unpiloted(&db, id, kind, owner, system).await.unwrap());
    }}).await;
    db.execute("DELETE FROM game.items WHERE item_id=$1", &[&id]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE character_id=$1", &[&owner]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await.unwrap();
    result.unwrap();
}
