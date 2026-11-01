use super::*;
use crate::consts::{CATEGORY_SHIP, FLAG_FITTING_FIRST, dogma::{ATTR_ARMOR_HP, ATTR_ARMOR_DAMAGE, ATTR_HULL_DAMAGE, ATTR_SHIELD_CHARGE}};
use crate::db::Db;
use crate::dao::{items::{creation, dogma}, skills::training};

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn unpiloted_ball_excludes_owner_skills_and_restores_fitted_condition() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::deployment::DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let hull = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP) && t.attribute(ATTR_ARMOR_HP).unwrap_or(0.0) > 100.0).unwrap().type_id;
    let plate = sde.types.iter().find(|t| t.name.as_deref() == Some("400mm Steel Plates I")).expect("armor plate in SDE").type_id;
    let skill = sde.types.iter().find(|t| t.name.as_deref() == Some("Hull Upgrades")).expect("Hull Upgrades in SDE").type_id;
    let station = sde.map.stations().next().unwrap().station_id;
    let (account, owner, ship) = db.transaction(
        |tx, _: &mut ()| Box::pin(async move {
            let account: i64 = tx.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES (nextval('game.accounts_user_id_seq')::text, ''::bytea, 0) RETURNING user_id", &[]).await?.get("user_id");
            let owner: i64 = tx.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id, station_id, balance) VALUES ($1, nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1, $2, 0) RETURNING character_id", &[&account, &station]).await?.get("character_id");
            let ship = creation::positioned(tx, hull, owner, station, [1.0, 2.0, 3.0]).await?;
            Ok::<_, crate::db::Error>((account, owner, ship))
        }),
        |value, ()| async move { Ok(value) },
    ).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
    let db = test_db;
    training::starting_skill(&db, owner, skill, 5, 256000).await.unwrap();
    let bare = Fit::load_unpiloted(&db, sde.clone(), ship).await.ok().unwrap().value(Key::Ship, ATTR_ARMOR_HP);
    creation::fitted_items(&db, ship, owner, &[(plate, FLAG_FITTING_FIRST, 1)]).await.unwrap();
    let fit = Fit::load_unpiloted(&db, sde.clone(), ship).await.ok().unwrap();
    let armor = fit.value(Key::Ship, ATTR_ARMOR_HP);
    assert!(armor > bare, "fitted plate must increase armor");
    assert!(fit.source_type(Key::Char).is_none());
    assert!(fit.source_type(Key::Skill(skill)).is_none());
    let piloted = Fit::load(&db, sde.clone(), owner, ship).await.ok().unwrap();
    assert!(piloted.value(Key::Ship, ATTR_ARMOR_HP) > armor, "owner skill must affect only piloted fit");
    dogma::save_condition(&db, ship, &[ATTR_SHIELD_CHARGE, ATTR_ARMOR_DAMAGE, ATTR_HULL_DAMAGE], &[0.0, armor / 2.0, 0.0], ATTR_SHIELD_CHARGE, 0).await.unwrap();
    let row = FreeShip { item_id: ship, type_id: hull, owner_id: owner, corporation_id: 1, alliance_id: 0, pos: [1.0, 2.0, 3.0] };
    let loser = combat::prepare_condition_with_fit(&db, ship, &fit).await.ok().unwrap();
    assert!((loser.fractions()[1] - 0.5).abs() < 1e-9);
    dogma::save_condition(&db, ship, &[ATTR_SHIELD_CHARGE, ATTR_ARMOR_DAMAGE, ATTR_HULL_DAMAGE], &[0.0, armor / 4.0, 0.0], ATTR_SHIELD_CHARGE, 0).await.unwrap();
    let prepared_pilot = super::piloted(&db, sde.clone(), &row, owner, 0.5).await.ok().unwrap();
    assert!(combat::ship_condition(ship).is_none());
    assert_eq!(prepared_pilot.ball.char_id, Some(owner));
    assert!(prepared_pilot.ball.interactive);
    assert!((prepared_pilot.ball.health[1] - (1.0 - armor / 4.0 / piloted.value(Key::Ship, ATTR_ARMOR_HP))).abs() < 1e-9);
    let prepared = unpiloted(&db, sde, &row).await.ok().unwrap();
    assert!(combat::ship_condition(ship).is_none());
    assert!((prepared.ball.health[1] - 0.75).abs() < 1e-9);
    let ball = prepared.install();
    assert!((combat::ship_condition(ship).unwrap().1 - armor / 4.0).abs() < 1e-9);
    dogma::save_condition(&db, ship, &[ATTR_SHIELD_CHARGE, ATTR_ARMOR_DAMAGE, ATTR_HULL_DAMAGE], &[0.0, armor / 2.0, 0.0], ATTR_SHIELD_CHARGE, 0).await.unwrap();
    let later = combat::prepare_condition_with_fit(&db, ship, &fit).await.ok().unwrap();
    assert!((later.fractions()[1] - 0.5).abs() < 1e-9);
    drop((loser, later));
    assert!((combat::ship_condition(ship).unwrap().1 - armor / 4.0).abs() < 1e-9);
    assert_eq!(ball.owner_id, owner);
    assert_eq!(ball.char_id, None);
    assert!(!ball.interactive);
    assert_eq!(ball.pos, row.pos);
    assert!((ball.health[1] - 0.75).abs() < 1e-9);
    }).await;
    combat::invalidate_condition(ship);
    db.execute("DELETE FROM game.item_attributes WHERE item_id = $1", &[&ship]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE location_id = $1", &[&ship]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id = $1", &[&ship]).await.unwrap();
    db.execute("DELETE FROM game.skills WHERE character_id = $1", &[&owner]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE character_id = $1", &[&owner]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&account]).await.unwrap();
    result.unwrap();
}
