//! Real Thermodynamics skill modifiers and overload eligibility.
use super::*;
use crate::{consts::{dogma::ATTR_SKILL_LEVEL, dogma::ATTR_SKILL_TIME_CONSTANT}, dao::skills::training, services::skills::formulas};

/// SDE Thermodynamics skill type.
const THERMODYNAMICS: i32 = 28164;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn thermodynamics_reduces_heat_damage_once_and_gates_overload() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let hull = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP)
        && t.attribute(MEDIUM_CAPACITY).is_some_and(|v| v > 0.0)).expect("published heat-capable hull").type_id;
    let module = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_MODULE)
        && t.attribute(HEAT_DAMAGE).is_some_and(|v| v > 0.0)
        && t.attribute(ATTR_STRUCTURE_HP).is_some_and(|v| v > 0.0)
        && t.attribute(REQUIRED_THERMODYNAMICS).is_some_and(|v| v > 0.0 && v <= 5.0)
        && t.effects().iter().any(|e| e.effect_id == EFFECT_MICROWARPDRIVE)
        && t.effects().iter().any(|e| e.effect_id == OVERLOAD_SPEED)).expect("overloadable propulsion module requiring Thermodynamics").type_id;
    let rank = sde.types.get(THERMODYNAMICS).and_then(|t| t.fit_attribute(ATTR_SKILL_TIME_CONSTANT)).expect("Thermodynamics rank");
    let before = fixture::fingerprint(&db).await.unwrap();
    let f = fixture::create(&db, hull, module, FLAG_MED_SLOT_FIRST).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let d = db.clone();
    let w = world.clone();
    let result = tokio::spawn(async move {
        fixture::position(&d, f.ship, SYSTEM, [0.0; 3]).await.unwrap();
        dogma::set_override(&d, f.module, HEAT_DAMAGE, 100.0).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, addr) = listener.accept().await.unwrap();
        let login = Login { user_id: f.account, user_name: f.account.to_string(), role: 0, client_id: f.character, session_id: f.character, language_id: "EN".into() };
        let mut c = Client::new(Conn::new(stream, addr), Arc::new(Config::default()), d.clone(), sde.clone(), crate::chat::Hub::new("localhost"), w.clone(), login);
        c.session.apply(vec![("charid", Value::Int(f.character)), ("shipid", Value::Int(f.ship)), ("solarsystemid", Value::Int(SYSTEM))]);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        c.notify_tx = tx.clone();
        w.register_pilot(f.character, tx);
        w.add_ship(SYSTEM, fixture::ball(f.ship, hull, f.character, Some(f.character), 0.0)).await.unwrap();
        let overload = [Value::Int(f.module), Value::Int(i64::from(OVERLOAD_SPEED))];
        let fit = Fit::load(&d, sde.clone(), f.character, f.ship).await.ok().unwrap();
        assert_eq!(fit.value(Key::Module(f.module), HEAT_DAMAGE), 100.0);
        assert_eq!(fit.value(Key::Skill(THERMODYNAMICS), ATTR_SKILL_LEVEL), 0.0);
        assert!(matches!(call(&mut c, "dogmaIM", "Overload", &overload, None).await,
            Err(services::CallError::User { msg, dict }) if msg == crate::messages::MSG_CUSTOM_NOTIFY
                && dict == vec![("notify".into(), Value::unicode(crate::messages::heat::THERMODYNAMICS_REQUIRED))]));
        assert!(!heat::overloaded_modules().contains(&f.module));
        for (level, expected) in [(1, 95.0), (5, 75.0)] {
            // Invert the existing level calculation instead of duplicating its skill-point formula.
            let (mut low, mut high) = (0_i64, 1_i64 << 40);
            while low < high {
                let mid = low + (high - low) / 2;
                if formulas::level_from_points(mid, rank) < level { low = mid + 1; } else { high = mid; }
            }
            assert_eq!(formulas::level_from_points(low, rank), level);
            training::set_level(&d, f.character, THERMODYNAMICS, level, low).await.unwrap();
            let fit = Fit::load(&d, sde.clone(), f.character, f.ship).await.ok().unwrap();
            assert_eq!(fit.value(Key::Skill(THERMODYNAMICS), ATTR_SKILL_LEVEL), f64::from(level));
            assert_eq!(fit.value(Key::Module(f.module), HEAT_DAMAGE), expected);
        }
        call(&mut c, "dogmaIM", "Overload", &overload, None).await.ok().expect("trained Thermodynamics permits overload");
        assert!(heat::overloaded_modules().contains(&f.module));
    }).await;
    heat::clear_character(f.character);
    world.remove_ball(SYSTEM, f.ship).await;
    world.unregister_pilot(f.character);
    combat::forget_ship(f.ship);
    let flush = world.flush_positions().await;
    drop(world);
    training::remove(&db, f.character, &[THERMODYNAMICS]).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
    assert_eq!(fixture::fingerprint(&db).await.unwrap(), before);
    flush.unwrap();
    result.unwrap();
}
