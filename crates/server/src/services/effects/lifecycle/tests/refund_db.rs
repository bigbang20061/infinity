use super::*;
use crate::{consts::{ATTR_CAPACITOR_CAPACITY, ATTR_RECHARGE_RATE, CATEGORY_SHIP}, dao::items::{combat, pending_reloads, creation}, services::combat::groups_health::{capacitor_snapshot, drain_capacitor}, space::{Ball, World, WarpState, MODE_STOP}};

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn refused_charge_consumption_refunds_debit() {
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let ship_type = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)
        && t.attribute(ATTR_CAPACITOR_CAPACITY).is_some_and(|v| v.is_finite() && v > 0.0)
        && t.attribute(ATTR_RECHARGE_RATE).is_some_and(|v| v.is_finite() && v > 0.0)).unwrap().type_id;
    let account: i64 = db.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES ('refund-fixture-' || nextval('game.accounts_user_id_seq')::text, $1, 0) RETURNING user_id", &[&&b"fixture"[..]]).await.unwrap().get("user_id");
    let character: i64 = db.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id) VALUES ($1, 'refund-fixture-' || nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1) RETURNING character_id", &[&account]).await.unwrap().get("character_id");
    let ship = creation::positioned(&db, ship_type, character, 1, [0.0; 3]).await.unwrap();
    let module = Module { id: ship, type_id: ship_type, flag: 1 };
    combat::apply_loads(&db, ship, &[(module.id, module.flag, ship_type, 1, 0)]).await.unwrap();
    let reload = pending_reloads::insert(&db, character, ship, module.id, module.flag, ship_type, 1).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let w = world.clone();
    let d = db.clone();
    let result = tokio::spawn(async move {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        w.register_pilot(character, tx.clone());
        let ball = Ball { id: ship, type_id: ship_type, owner_id: character, corp_id: 1, alliance_id: -1,
            char_id: Some(character), signature_radius: None, mode: MODE_STOP, radius: 1.0, mass: 1e6,
            max_vel: 0.0, agility: 1.0, speed_fraction: 0.0, pos: [0.0; 3], vel: [0.0; 3],
            old_pos: [0.0; 3], old_vel: [0.0; 3], segment_acceleration: None, goto: [0.0; 3],
            follow_id: 0, follow_range: 0.0, free: false, global: false, massive: false, interactive: false,
            warp: WarpState::default(), warp_factor: 0, last_ms: 0, interpolation_anchor_ms: 0,
            effect_stamp: 0, cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new() };
        w.add_ship(1, ball).await.unwrap();
        let cx = Ctx { db: d.clone(), world: w.clone(), notify: tx, char_id: character, ship_id: ship, system_id: 1 };
        let fit = Fit::load(&d, sde, character, ship).await.ok().expect("load fit");
        let capacity = fit.value(Key::Ship, ATTR_CAPACITOR_CAPACITY);
        let debit = capacity / 2.0;
        assert!(drain_capacitor(&cx, &fit, debit));
        assert_eq!(consume_charge(&cx, &module, Some(ship_type)).await.ok().expect("consume during reload"), None);
        refund_refused_charge(&cx, debit).await.ok().expect("refund capacitor");
        let level = capacitor_snapshot(&d, &w, 1, ship).await.ok().expect("snapshot capacitor").unwrap().level;
        assert!((level - capacity).abs() < 1e-6);
        assert_eq!(loaded(&d, ship, module.flag).await.ok().expect("read loaded charge"), Some((ship_type, 1)));
        pending_reloads::take(&d, reload).await.unwrap();
        assert_eq!(consume_charge(&cx, &module, Some(ship_type + 1)).await.ok().expect("consume mismatched charge"), None);
        assert_eq!(loaded(&d, ship, module.flag).await.ok().expect("read loaded charge"), Some((ship_type, 1)));
    }).await;
    world.remove_ball(1, ship).await;
    world.unregister_pilot(character);
    world.flush_positions().await.unwrap();
    crate::services::combat::forget_ship(ship);
    pending_reloads::take(&db, reload).await.unwrap();
    combat::delete_charge(&db, ship, 1).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id = $1", &[&ship]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE character_id = $1", &[&character]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&account]).await.unwrap();
    result.unwrap();
}
