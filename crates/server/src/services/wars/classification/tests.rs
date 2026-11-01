//! Transactional hostility and immutable kill-side classification.
use super::*;

#[test]
fn minimum_projection_handles_multiple_wars_and_missing_kills() {
    let projections = vec![
        projections::Projection { kill_id: 1, war_id: 9, attacker_side: 2, defender_side: 3 },
        projections::Projection { kill_id: 1, war_id: 7, attacker_side: 2, defender_side: 3 },
        projections::Projection { kill_id: 2, war_id: 8, attacker_side: 3, defender_side: 2 },
    ];
    let result = crate::services::crimewatch::killmails::minimum_wars(&projections);
    assert_eq!(result.get(&1), Some(&7));
    assert_eq!(result.get(&2), Some(&8));
    assert_eq!(result.get(&3), None);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn boundaries_multiple_wars_and_affiliation_history() {
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let account: i64 = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES (nextval('game.accounts_user_id_seq')::text,''::bytea,0) RETURNING user_id", &[]).await.unwrap().get("user_id");
    let mut characters = Vec::new();
    let mut corps = Vec::new();
    for _ in 0..4 {
        let character: i64 = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,nextval('game.characters_character_id_seq')::text,1,1,1,1) RETURNING character_id", &[&account]).await.unwrap().get("character_id");
        let corp: i64 = tx.query_one("INSERT INTO game.corporations (name,ticker,ceo_id,creator_id,station_id,created) VALUES (nextval('game.corporations_corporation_id_seq')::text,nextval('game.corporations_corporation_id_seq')::text,$1,$1,1,1) RETURNING corporation_id", &[&character]).await.unwrap().get("corporation_id");
        tx.execute("UPDATE game.characters SET corporation_id=$1 WHERE character_id=$2", &[&corp, &character]).await.unwrap();
        characters.push(character);
        corps.push(corp);
    }
    let alliance: i64 = tx.query_one("INSERT INTO game.alliances (name,short_name,executor_corp_id,creator_corp_id,creator_char_id,created) VALUES (nextval('game.alliances_alliance_id_seq')::text,nextval('game.alliances_alliance_id_seq')::text,$1,$1,$2,1) RETURNING alliance_id", &[&corps[0], &characters[0]]).await.unwrap().get("alliance_id");
    let old = wars::create(&tx, corps[0], corps[1], 1, 3).await.unwrap().unwrap();
    assert!(!legal(&tx, characters[1], characters[0], 3).await.unwrap());
    assert!(legal(&tx, characters[1], characters[0], 4).await.unwrap());
    tx.execute("UPDATE game.corporations SET alliance_id=$1 WHERE corporation_id=$2", &[&alliance, &corps[0]]).await.unwrap();
    assert!(!legal(&tx, characters[1], characters[0], 4).await.unwrap());
    let first = wars::create(&tx, alliance, corps[1], 1, 3).await.unwrap().unwrap();
    let second = wars::create(&tx, corps[2], alliance, 1, 3).await.unwrap().unwrap();
    assert!(legal(&tx, characters[1], characters[0], 4).await.unwrap());
    assert!(!legal(&tx, characters[0], characters[0], 4).await.unwrap());
    let kill: i64 = tx.query_one("INSERT INTO game.killmails (kill_time,solar_system_id,victim_id,victim_corp_id,victim_alliance_id,victim_ship_type,damage_taken,final_ship_type,final_weapon_type,final_damage,final_sec_status,isk_lost,attacker_ids,blob) VALUES (4,1,$1,$2,$3,1,0,1,1,0,0,0,ARRAY[]::bigint[],'') RETURNING kill_id", &[&characters[0], &corps[0], &alliance]).await.unwrap().get("kill_id");
    let attackers: Vec<_> = [1, 2, 3, 1].into_iter().map(|i| Affiliation { character: characters[i], corporation: corps[i], alliance: None }).collect();
    index(&tx, kill, corps[0], Some(alliance), &attackers, 4).await.unwrap();
    let indexed = projections::by_kills(&tx, &[kill]).await.unwrap();
    let minimum = crate::services::crimewatch::killmails::minimum_wars(&indexed);
    assert_eq!(minimum.get(&kill), Some(&first.war_id.min(second.war_id)));
    assert_eq!(minimum.get(&-kill), None);
    assert_eq!(indexed.len(), 2);
    assert!(indexed.iter().any(|p| p.war_id == first.war_id && p.attacker_side == corps[1] && p.defender_side == alliance));
    assert!(indexed.iter().any(|p| p.war_id == second.war_id && p.attacker_side == corps[2] && p.defender_side == alliance));
    assert!(indexed.iter().all(|p| p.war_id != old.war_id));
    tx.execute("UPDATE game.corporations SET alliance_id=NULL WHERE corporation_id=$1", &[&corps[0]]).await.unwrap();
    let history = projections::killmails(&tx, first.war_id).await.unwrap();
    assert_eq!(history[0].killmail.victim_alliance_id, Some(alliance));
    assert_eq!(history[0].killmail.final_alliance_id, None);
    assert_eq!(history[0].projection.defender_side, alliance);
    wars::retract(&tx, old.war_id, corps[0], 4, 6).await.unwrap().unwrap();
    assert!(legal(&tx, characters[1], characters[0], 5).await.unwrap());
    assert!(!legal(&tx, characters[1], characters[0], 6).await.unwrap());
    tx.rollback().await.unwrap();
}
