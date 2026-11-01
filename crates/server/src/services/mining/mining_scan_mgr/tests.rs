use super::*;

#[test]
fn survey_range_uses_largest_valid_fitted_scanner_or_integrated_range() {
    assert_eq!(scanner_range(42.0, [].into_iter()), 42.0);
    assert_eq!(scanner_range(42.0, [0.0, f64::NAN, 12.0, 23.0].into_iter()), 23.0);
    assert_eq!(scanner_range(42.0, [f64::INFINITY, -1.0].into_iter()), 42.0);
}

async fn fingerprint(db: &crate::db::Db) -> Vec<String> {
    let mut result = Vec::new();
    for table in ["accounts", "characters", "items", "item_attributes", "asteroids"] {
        let sql = format!("SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY row_to_json(t)::text), '')) AS fingerprint FROM game.{table} t");
        result.push(db.query_one(&sql, &[]).await.unwrap().get("fingerprint"));
    }
    result
}

#[tokio::test]
#[ignore = "requires a dedicated INFINITY_TEST_DATABASE_URL with loaded SDE; run serially"]
async fn perform_scan_uses_online_survey_range_then_integrated_range() {
    use crate::{consts::{CATEGORY_MODULE, CATEGORY_SHIP, FLAG_HI_SLOT_FIRST}, dao::items::{combat, dogma}, services::effects::{registry, EffectKind}};
    crate::tests::init_config();
    let mut config = crate::config::tests::test_database();
    config.schema_on_start = false;
    let db = Db::connect(&config).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let hull = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
    let scanner = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_MODULE)
        && t.effects().iter().any(|e| registry::kind(&sde, e.effect_id) == Some(EffectKind::SurveyScan)))
        .expect("SDE survey scanner").type_id;
    let ore = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_ASTEROID)).expect("SDE asteroid").type_id;
    let configured = crate::config::game().mining.scanner_range_m;
    assert!(configured.is_finite() && configured > 0.0);
    let before = fingerprint(&db).await;
    let (account, character, ship, module): (i64, i64, i64, i64) = db.transaction(
        move |tx, _: &mut ()| Box::pin(async move {
            let account = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ('survey-fixture-' || nextval('game.accounts_user_id_seq')::text,$1,0) RETURNING user_id", &[&&b"fixture"[..]]).await?.get("user_id");
            let character = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,'survey-fixture-' || nextval('game.characters_character_id_seq')::text,1,1,1,1) RETURNING character_id", &[&account]).await?.get("character_id");
            let ship = tx.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,x,y,z) VALUES ($1,$2,1,0,1,true,0,0,0) RETURNING item_id", &[&hull, &character]).await?.get("item_id");
            let module = tx.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,online) VALUES ($1,$2,$3,$4,1,true,true) RETURNING item_id", &[&scanner, &character, &ship, &FLAG_HI_SLOT_FIRST]).await?.get("item_id");
            Ok::<_, crate::db::Error>((account, character, ship, module))
        }), |fixture, ()| async move { Ok(fixture) }).await.unwrap();
    let system = -character;
    let inner = -ship;
    let outer = -module;
    let world = World::new(db.clone(), sde.clone(), 1);
    let test_world = world.clone();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        assert!(dogma::set_override(&test_db, module, ATTR_SURVEY_SCAN_RANGE, configured * 0.75).await.unwrap());
        test_world.system(system).await.unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        test_world.register_pilot(character, tx);
        let ship_ball = rock_ball(ship, hull, [0.0; 3], 1.0);
        let inner_ball = rock_ball(inner, ore, [configured * 0.5, 0.0, 0.0], 1.0);
        let outer_ball = rock_ball(outer, ore, [configured * 0.9, 0.0, 0.0], 1.0);
        test_world.with(system, move |s| { s.insert(ship_ball); s.insert(inner_ball); s.insert(outer_ball); }).await.unwrap();
        {
            let mut state = state();
            assert!(state.rocks.insert(inner, Rock { system, field: system, type_id: ore, quantity: 17 }).is_none());
            assert!(state.rocks.insert(outer, Rock { system, field: system, type_id: ore, quantity: 29 }).is_none());
        }
        let cfg = Arc::new(crate::config::Config::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, address) = listener.accept().await.unwrap();
        let login = crate::handshake::Login { user_id: account, user_name: "test".into(), role: 0, client_id: 1, session_id: 1, language_id: "EN".into() };
        let chat = crate::chat::Hub::new(&cfg.chat.domain, test_db.clone(), test_world.clone());
        let mut client = Client::new(crate::conn::Conn::new(stream, address), cfg, test_db.clone(), sde, chat, test_world, login);
        client.session.apply(vec![("charid", Value::Int(character)), ("shipid", Value::Int(ship)), ("solarsystemid", Value::Int(system))]);
        let expected_inner = Value::tuple([Value::Int(inner), Value::Int(ore as i64), Value::Int(17)]);
        let expected_outer = Value::tuple([Value::Int(outer), Value::Int(ore as i64), Value::Int(29)]);
        assert_eq!(perform_scan(&client).await, Value::List(vec![expected_inner.clone()]));
        assert_eq!(combat::set_online(&test_db, module, ship, character, FLAG_HI_SLOT_FIRST, scanner, hull, 1, 0, true, false).await.unwrap(), 1);
        let Value::List(rows) = perform_scan(&client).await else { panic!("expected scan results") };
        assert_eq!(rows.len(), 2);
        assert!(rows.contains(&expected_inner) && rows.contains(&expected_outer));
    }).await;
    world.remove_ball(system, inner).await;
    world.remove_ball(system, outer).await;
    world.remove_ball(system, ship).await;
    {
        let mut state = state();
        state.rocks.remove(&inner);
        state.rocks.remove(&outer);
    }
    world.unregister_pilot(character);
    let flush = world.flush_positions().await;
    drop(world);
    db.transaction(move |tx, _: &mut ()| Box::pin(async move {
        tx.execute("DELETE FROM game.item_attributes WHERE item_id IN (SELECT item_id FROM game.items WHERE owner_id=$1)", &[&character]).await?;
        tx.execute("DELETE FROM game.items WHERE owner_id=$1", &[&character]).await?;
        tx.execute("DELETE FROM game.characters WHERE character_id=$1 AND user_id=$2", &[&character, &account]).await?;
        tx.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await?;
        Ok::<_, crate::db::Error>(())
    }), |(), ()| async move { Ok(()) }).await.unwrap();
    assert_eq!(fingerprint(&db).await, before);
    flush.unwrap();
    result.unwrap();
}
