//! Skill trading boundaries and database settlement regressions.
use super::*;
mod reconciliation;

async fn fingerprint(db: &crate::db::Db, character: i64) -> String {
    let job = format!("skills.complete:{character}");
    db.query_one("SELECT (SELECT row_to_json(c)::text FROM game.characters c WHERE character_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(s)::text, '' ORDER BY type_id), '') FROM game.skills s WHERE character_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(q)::text, '' ORDER BY position), '') FROM game.skill_queue q WHERE character_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(i)::text, '' ORDER BY item_id), '') FROM game.items i WHERE owner_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(a)::text, '' ORDER BY a.item_id,a.attribute_id), '') FROM game.item_attributes a JOIN game.items i ON i.item_id=a.item_id WHERE i.owner_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(h)::text, '' ORDER BY history_id), '') FROM game.skill_history h WHERE character_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(j)::text, '' ORDER BY job_id), '') FROM game.jobs j WHERE job_id=$2) AS fingerprint", &[&character, &job]).await.unwrap().get("fingerprint")
}

fn notification_events(outbox: Outbox) -> Vec<String> {
    use crate::db::outbox::{Effect, Notification};
    outbox.into_entries().into_iter().filter_map(|entry| match entry {
        Effect::Notification(Notification::DirectClient { event, .. } | Notification::Pilot { event, .. }) => Some(event),
        Effect::Notification(Notification::Sender { .. }) => None,
        Effect::Event(crate::lua::events::GameEvent::SkillTrained { .. }) => panic!("manual item operation trained a skill"),
        Effect::Event(_) => None,
        Effect::Operation(never) => match never {},
    }).collect()
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn injector_rates_crossings_credits_and_limits() {
    use crate::services::tests::{client, fixture};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (c, _peer) = client(db.clone(), f.character).await;
    let calc = |ty, qty, total, credits| injector_yield(&c.sde, ty, qty, total, credits).unwrap_or_else(|_| panic!("unexpected refusal"));
    assert_eq!(calc(FULL_INJECTOR, 2, FREE_ZONE - 1, 1), (1_000_000, 1));
    assert_eq!(calc(FULL_INJECTOR, 2, LOW_BOUNDARY - 1, 0), (700_000, 0));
    assert_eq!(calc(FULL_INJECTOR, 2, MEDIUM_BOUNDARY - 1, 0), (450_000, 0));
    assert_eq!(calc(SMALL_INJECTOR, 1, MEDIUM_BOUNDARY, 0), (30_000, 0));
    assert!(injector_yield(&c.sde, FULL_INJECTOR, MAX_STACK + 1, 0, 0).is_err());
    assert!(injector_yield(&c.sde, FULL_INJECTOR, 0, 0, 0).is_err());
    if let (Some(points), Some(limit)) = (c.sde.types.attribute(MINI_INJECTOR, CONTAINED_POINTS), c.sde.types.attribute(MINI_INJECTOR, MAX_CHARACTER_POINTS)) {
        assert_eq!(calc(MINI_INJECTOR, 1, 0, 1), (points as i64, 0));
        assert!(injector_yield(&c.sde, MINI_INJECTOR, 1, limit as i64, 0).is_err());
    }
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn extraction_failure_rolls_back_and_success_consumes_once() {
    use crate::services::tests::{client, fixture};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (mut c, peer) = client(db.clone(), f.character).await;
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    c.world.register_pilot(f.character, sender);
    let skill = c.sde.types.iter().find(|t| t.category_id == Some(crate::consts::CATEGORY_SKILL) && t.published == Some(true) && super::super::formulas::sp_for_level(super::super::training::skill_type(&c.sde, t.type_id).rank, i64::from(crate::consts::MAX_SKILL_LEVEL)) >= BUCKET).expect("published skill").type_id;
    let filler = c.sde.types.iter().find(|t| t.type_id != skill && t.category_id == Some(crate::consts::CATEGORY_SKILL) && t.published == Some(true) && super::super::requirements::requirements(&c.sde, t.type_id).next().is_none()).expect("filler skill").type_id;
    let station = 60000001_i64;
    db.execute("UPDATE game.characters SET station_id=$2 WHERE character_id=$1", &[&f.character, &station]).await.unwrap();
    db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,5,$3),($1,$4,5,$5)", &[&f.character, &skill, &BUCKET, &filler, &FREE_ZONE]).await.unwrap();
    let item = transfers::insert(&db, EXTRACTOR, f.character, station, FLAG_HANGAR, 1).await.unwrap();
    let args = vec![Value::Dict(vec![(Value::Int(i64::from(skill)), Value::Int(BUCKET))]), Value::Int(item), Value::Int(1)];
    let invalid = vec![Value::Dict(vec![(Value::Int(i64::from(skill)), Value::Int(BUCKET - 1))]), Value::Int(item), Value::Int(1)];
    let before_invalid = fingerprint(&db, f.character).await;
    assert!(extract(&mut c, f.character, &invalid).await.is_err());
    assert_eq!(fingerprint(&db, f.character).await, before_invalid);
    assert!(receiver.try_recv().is_err());
    db.execute("UPDATE game.skills SET skill_points=$3 WHERE character_id=$1 AND type_id=$2", &[&f.character, &filler, &(FREE_ZONE - 1)]).await.unwrap();
    assert!(extract(&mut c, f.character, &args).await.is_err());
    db.execute("UPDATE game.skills SET skill_points=$3 WHERE character_id=$1 AND type_id=$2", &[&f.character, &filler, &FREE_ZONE]).await.unwrap();
    db.execute("INSERT INTO game.skill_queue (character_id,position,type_id,to_level,start_sp,dest_sp) VALUES ($1,0,$2,1,0,100)", &[&f.character, &skill]).await.unwrap();
    assert!(extract(&mut c, f.character, &args).await.is_err());
    db.execute("DELETE FROM game.skill_queue WHERE character_id=$1", &[&f.character]).await.unwrap();
    let before = fingerprint(&db, f.character).await;
    fixture::install_skill_failure(&db, f.character).await.unwrap();
    assert!(extract(&mut c, f.character, &args).await.is_err());
    fixture::remove_skill_failure(&db, f.character).await.unwrap();
    assert_eq!(fingerprint(&db, f.character).await, before);
    assert!(receiver.try_recv().is_err());
    assert_eq!(db.query_one("SELECT quantity FROM game.items WHERE item_id=$1", &[&item]).await.unwrap().get::<_, i32>("quantity"), 1);
    assert!(crate::dao::skills::history::list(&db, f.character, 50).await.unwrap().is_empty());
    let mut buffer = [0_u8; 1];
    assert!(matches!(peer.try_read(&mut buffer), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock));
    let output: i64 = db.query_one("SELECT count(*) AS count FROM game.items WHERE owner_id=$1 AND type_id=$2", &[&f.character, &FULL_INJECTOR]).await.unwrap().get("count");
    assert_eq!(output, 0);
    let sde = c.sde.clone();
    let selected = BTreeMap::from([(skill, BUCKET)]);
    let events = db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
        let sde = sde.clone();
        let selected = selected.clone();
        Box::pin(async move { extract_tx(tx, outbox, &sde, f.character, None, item, crate::client::filetime_now(), &selected).await })
    }, |_, outbox| { let c = &mut c; async move {
        use crate::db::outbox::{Effect, Notification};
        let mut events = Vec::new();
        for entry in outbox.into_entries() {
            match entry {
                Effect::Notification(notification) => {
                    match &notification {
                        Notification::DirectClient { event, .. } | Notification::Pilot { event, .. } => events.push(event.clone()),
                        Notification::Sender { .. } => {}
                    }
                    notification.dispatch(&c.world.clone(), Some(&mut *c))?;
                }
                Effect::Event(crate::lua::events::GameEvent::SkillTrained { .. }) => panic!("manual extraction trained a skill"),
                Effect::Event(_) => {}
                Effect::Operation(change) => online::apply_online_changed(change),
            }
        }
        Ok(events)
    }}).await.unwrap_or_else(|_| panic!("extraction failed"));
    assert_eq!(events.iter().filter(|event| event.as_str() == "OnItemChange").count(), 2);
    let mut events = Vec::new();
    while let Ok((event, _)) = receiver.try_recv() { events.push(event); }
    assert_eq!(events.iter().filter(|event| event.as_str() == "OnServerSkillsChanged").count(), 1);
    assert_eq!(events.iter().filter(|event| event.as_str() == crate::client::BRAIN_REFRESH).count(), 1);
    assert!(extract(&mut c, f.character, &args).await.is_err());
    let output: i64 = db.query_one("SELECT count(*) AS count FROM game.items WHERE owner_id=$1 AND type_id=$2", &[&f.character, &FULL_INJECTOR]).await.unwrap().get("count");
    assert_eq!(output, 1);
    assert_eq!(crate::dao::skills::history::list(&db, f.character, 50).await.unwrap().len(), 1);
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn conversion_and_injection_conserve_items_and_refuse_wrong_sources() {
    use crate::services::tests::{client, fixture};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (mut c, _peer) = client(db.clone(), f.character).await;
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    c.world.register_pilot(f.character, sender);
    fixture::seed_skill(&db, f.character).await.unwrap();
    let station = 60000001_i64;
    db.execute("UPDATE game.characters SET station_id=$2 WHERE character_id=$1", &[&f.character, &station]).await.unwrap();
    let item = transfers::insert(&db, FULL_INJECTOR, f.character, station, FLAG_HANGAR, 2).await.unwrap();
    let args = vec![Value::Int(item), Value::Int(1)];
    assert!(inject(&mut c, f.character, &[Value::Int(item), Value::Int(3)], false).await.is_err());
    assert!(convert(&mut c, f.character, &[Value::Int(item), Value::Int(0)], true).await.is_err());
    convert(&mut c, f.character, &args, true).await.unwrap_or_else(|_| panic!("split failed"));
    let mut events = Vec::new();
    while let Ok((event, _)) = receiver.try_recv() { events.push(event); }
    assert_eq!(events.iter().filter(|event| event.as_str() == "OnServerSkillsChanged").count(), 1);
    assert_eq!(events.iter().filter(|event| event.as_str() == crate::client::BRAIN_REFRESH).count(), 1);
    assert_eq!(events.iter().filter(|event| event.as_str() == "OnNewSkillQueueSaved").count(), 1);
    assert!(crate::dao::skills::queue::list(&db, f.character).await.unwrap().is_empty());
    let small: i64 = db.query_one("SELECT item_id FROM game.items WHERE owner_id=$1 AND type_id=$2", &[&f.character, &SMALL_INJECTOR]).await.unwrap().get("item_id");
    let before = fingerprint(&db, f.character).await;
    assert!(matches!(convert(&mut c, f.character, &[Value::Int(small), Value::Int(4)], false).await, Err(CallError::User { msg, .. }) if msg == messages::MSG_COMBINE_SKILL_INJECTOR_TOO_FEW_INJECTORS));
    assert_eq!(fingerprint(&db, f.character).await, before);
    convert(&mut c, f.character, &[Value::Int(small), Value::Int(5)], false).await.unwrap_or_else(|_| panic!("combine failed"));
    let quantity: i64 = db.query_one("SELECT sum(quantity)::bigint AS quantity FROM game.items WHERE owner_id=$1 AND type_id=$2", &[&f.character, &FULL_INJECTOR]).await.unwrap().get("quantity");
    assert_eq!(quantity, 2);
    let quote = quote(&c, f.character, &[Value::Int(i64::from(FULL_INJECTOR)), Value::Int(1), Value::Int(999)]).await.unwrap_or_else(|_| panic!("quote failed"));
    inject(&mut c, f.character, &args, true).await.unwrap_or_else(|_| panic!("constraint check failed"));
    inject(&mut c, f.character, &args, false).await.unwrap_or_else(|_| panic!("injection failed"));
    assert_eq!(crate::dao::characters::attributes::free_points(&db, f.character).await.unwrap(), quote.as_int().unwrap());
    assert!(inject(&mut c, f.character, &args, false).await.is_err());
    let remote = transfers::insert(&db, FULL_INJECTOR, f.character, station + 1, FLAG_HANGAR, 1).await.unwrap();
    assert!(inject(&mut c, f.character, &[Value::Int(remote), Value::Int(1)], false).await.is_err());
    let seven = transfers::insert(&db, SMALL_INJECTOR, f.character, station, FLAG_HANGAR, 7).await.unwrap();
    convert(&mut c, f.character, &[Value::Int(seven), Value::Int(7)], false).await.unwrap_or_else(|_| panic!("combine seven failed"));
    assert_eq!(db.query_one("SELECT quantity FROM game.items WHERE item_id=$1", &[&seven]).await.unwrap().get::<_, i32>("quantity"), 2);
    let quantity: i64 = db.query_one("SELECT sum(quantity)::bigint AS quantity FROM game.items WHERE owner_id=$1 AND type_id=$2 AND location_id=$3", &[&f.character, &FULL_INJECTOR, &station]).await.unwrap().get("quantity");
    assert_eq!(quantity, 2);
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn credit_override_zero_is_authoritative() {
    use crate::services::tests::{client, fixture};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (c, _peer) = client(db.clone(), f.character).await;
    let booster = c.sde.types.iter().find(|t| c.sde.types.attribute(t.type_id, CREDIT_ATTRIBUTE).is_some_and(|value| value >= 1.0 && value.fract() == 0.0)).expect("credit booster in SDE");
    let item = transfers::insert(&db, booster.type_id, f.character, f.character, crate::consts::FLAG_BOOSTER, 1).await.unwrap();
    assert!(available_credits(&db, &c.sde, f.character).await.unwrap_or_else(|_| panic!("credit read failed")) > 0);
    db.execute("INSERT INTO game.item_attributes (item_id, attribute_id, value) VALUES ($1,$2,0)", &[&item, &CREDIT_ATTRIBUTE]).await.unwrap();
    assert_eq!(available_credits(&db, &c.sde, f.character).await.unwrap_or_else(|_| panic!("credit read failed")), 0);
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn extraction_refuses_broken_dependent_prerequisite() {
    use crate::services::tests::{client, fixture};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (mut c, _peer) = client(db.clone(), f.character).await;
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    c.world.register_pilot(f.character, sender);
    let (dependent, required, threshold) = c.sde.types.iter().filter(|t| t.category_id == Some(crate::consts::CATEGORY_SKILL) && t.published == Some(true)).find_map(|ty| {
        super::super::requirements::requirements(&c.sde, ty.type_id).find_map(|(required, level)| {
            let target = c.sde.types.get(required)?;
            let threshold = super::super::formulas::sp_for_level(super::super::training::skill_type(&c.sde, required).rank, i64::from(level));
            (required != ty.type_id && target.published == Some(true) && threshold >= BUCKET).then_some((ty.type_id, required, threshold))
        })
    }).expect("SDE prerequisite with enough points");
    let station = 60000001_i64;
    db.execute("UPDATE game.characters SET station_id=$2 WHERE character_id=$1", &[&f.character, &station]).await.unwrap();
    db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,5,$3),($1,$4,1,$5)", &[&f.character, &required, &threshold, &dependent, &EXTRACTION_MINIMUM]).await.unwrap();
    let item = transfers::insert(&db, EXTRACTOR, f.character, station, FLAG_HANGAR, 1).await.unwrap();
    let before = fingerprint(&db, f.character).await;
    let args = [Value::Dict(vec![(Value::Int(i64::from(required)), Value::Int(BUCKET))]), Value::Int(item), Value::Int(1)];
    assert!(extract(&mut c, f.character, &args).await.is_err());
    assert_eq!(fingerprint(&db, f.character).await, before);
    assert!(receiver.try_recv().is_err());
    assert_eq!(db.query_one("SELECT quantity FROM game.items WHERE item_id=$1", &[&item]).await.unwrap().get::<_, i32>("quantity"), 1);
    assert!(crate::dao::skills::history::list(&db, f.character, 50).await.unwrap().is_empty());
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn extraction_refuses_equipped_item_prerequisite() {
    use crate::services::tests::{client, fixture};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (mut c, _peer) = client(db.clone(), f.character).await;
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    c.world.register_pilot(f.character, sender);
    let (equipment, required, threshold) = c.sde.types.iter().filter(|t| t.category_id != Some(crate::consts::CATEGORY_SKILL)).find_map(|ty| {
        super::super::requirements::requirements(&c.sde, ty.type_id).find_map(|(required, level)| {
            let target = c.sde.types.get(required)?;
            let threshold = super::super::formulas::sp_for_level(super::super::training::skill_type(&c.sde, required).rank, i64::from(level));
            (target.published == Some(true) && threshold >= BUCKET).then_some((ty.type_id, required, threshold))
        })
    }).expect("SDE equipment prerequisite with enough points");
    let filler = c.sde.types.iter().find(|t| t.type_id != required && t.category_id == Some(crate::consts::CATEGORY_SKILL) && t.published == Some(true)).expect("filler skill").type_id;
    let station = 60000001_i64;
    db.execute("UPDATE game.characters SET station_id=$2 WHERE character_id=$1", &[&f.character, &station]).await.unwrap();
    db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,5,$3),($1,$4,5,$5)", &[&f.character, &required, &threshold, &filler, &EXTRACTION_MINIMUM]).await.unwrap();
    transfers::insert(&db, equipment, f.character, f.character, crate::consts::FLAG_IMPLANT, 1).await.unwrap();
    let item = transfers::insert(&db, EXTRACTOR, f.character, station, FLAG_HANGAR, 1).await.unwrap();
    let before = fingerprint(&db, f.character).await;
    let args = [Value::Dict(vec![(Value::Int(i64::from(required)), Value::Int(BUCKET))]), Value::Int(item), Value::Int(1)];
    assert!(extract(&mut c, f.character, &args).await.is_err());
    assert_eq!(fingerprint(&db, f.character).await, before);
    assert!(receiver.try_recv().is_err());
    assert_eq!(db.query_one("SELECT quantity FROM game.items WHERE item_id=$1", &[&item]).await.unwrap().get::<_, i32>("quantity"), 1);
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn extraction_refuses_clone_floor_and_overtraining_atomically() {
    use crate::services::tests::{client, fixture};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (mut c, peer) = client(db.clone(), f.character).await;
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    c.world.register_pilot(f.character, sender);
    let clone_skill = crate::consts::SKILL_INFOMORPH_PSYCHOLOGY;
    assert!(c.sde.types.get(clone_skill).is_some(), "clone skill");
    let filler = c.sde.types.iter().find(|t| t.type_id != clone_skill && t.category_id == Some(crate::consts::CATEGORY_SKILL) && t.published == Some(true) && super::super::requirements::requirements(&c.sde, t.type_id).next().is_none() && super::super::formulas::sp_for_level(super::super::training::skill_type(&c.sde, t.type_id).rank, i64::from(crate::consts::MAX_SKILL_LEVEL)) >= BUCKET).expect("filler skill").type_id;
    let reserve = c.sde.types.iter().find(|t| t.type_id != clone_skill && t.type_id != filler && t.category_id == Some(crate::consts::CATEGORY_SKILL) && t.published == Some(true) && super::super::requirements::requirements(&c.sde, t.type_id).next().is_none()).expect("reserve skill").type_id;
    let station = 60000001_i64;
    db.execute("UPDATE game.characters SET station_id=$2 WHERE character_id=$1", &[&f.character, &station]).await.unwrap();
    let clone_points = super::super::formulas::sp_for_level(super::super::training::skill_type(&c.sde, clone_skill).rank, 1);
    db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,1,$3),($1,$4,5,$5),($1,$6,5,$7)", &[&f.character, &clone_skill, &clone_points, &filler, &BUCKET, &reserve, &FREE_ZONE]).await.unwrap();
    let item = transfers::insert(&db, EXTRACTOR, f.character, station, FLAG_HANGAR, 1).await.unwrap();
    crate::dao::characters::clones::install(&db, f.character, station).await.unwrap();
    let args = [Value::Dict(vec![(Value::Int(i64::from(clone_skill)), Value::Int(1)), (Value::Int(i64::from(filler)), Value::Int(BUCKET - 1))]), Value::Int(item), Value::Int(1)];
    let before = fingerprint(&db, f.character).await;
    assert!(extract(&mut c, f.character, &args).await.is_err());
    assert_eq!(fingerprint(&db, f.character).await, before);
    assert!(receiver.try_recv().is_err());
    for (tier, count) in [(crate::consts::SKILL_ADVANCED_INFOMORPH_PSYCHOLOGY, 6_i64), (crate::consts::SKILL_ELITE_INFOMORPH_PSYCHOLOGY, 11_i64)] {
        if c.sde.types.get(tier).is_none() { continue; }
        for index in 1..count {
            crate::dao::characters::clones::install(&db, f.character, station + index).await.unwrap();
        }
        let points = super::super::formulas::sp_for_level(super::super::training::skill_type(&c.sde, tier).rank, 1);
        db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,1,$3)", &[&f.character, &tier, &points]).await.unwrap();
        let args = [Value::Dict(vec![(Value::Int(i64::from(tier)), Value::Int(1)), (Value::Int(i64::from(filler)), Value::Int(BUCKET - 1))]), Value::Int(item), Value::Int(1)];
        let before = fingerprint(&db, f.character).await;
        assert!(extract(&mut c, f.character, &args).await.is_err());
        assert_eq!(fingerprint(&db, f.character).await, before);
        assert!(receiver.try_recv().is_err());
        db.execute("DELETE FROM game.skills WHERE character_id=$1 AND type_id=$2", &[&f.character, &tier]).await.unwrap();
    }
    db.execute("DELETE FROM game.jump_clones WHERE owner_id=$1", &[&f.character]).await.unwrap();
    let cap = super::super::formulas::sp_for_level(super::super::training::skill_type(&c.sde, filler).rank, i64::from(crate::consts::MAX_SKILL_LEVEL));
    db.execute("UPDATE game.skills SET skill_points=$3 WHERE character_id=$1 AND type_id=$2", &[&f.character, &filler, &(cap + 1)]).await.unwrap();
    let args = [Value::Dict(vec![(Value::Int(i64::from(filler)), Value::Int(BUCKET))]), Value::Int(item), Value::Int(1)];
    let before = fingerprint(&db, f.character).await;
    assert!(extract(&mut c, f.character, &args).await.is_err());
    assert_eq!(fingerprint(&db, f.character).await, before);
    assert!(receiver.try_recv().is_err());
    let mut buffer = [0_u8; 1];
    assert!(matches!(peer.try_read(&mut buffer), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock));
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn injection_and_conversion_outboxes_are_exact() {
    use crate::services::tests::{client, fixture};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (c, _peer) = client(db.clone(), f.character).await;
    let station = 60000001_i64;
    db.execute("UPDATE game.characters SET station_id=$2 WHERE character_id=$1", &[&f.character, &station]).await.unwrap();
    let item = transfers::insert(&db, FULL_INJECTOR, f.character, station, FLAG_HANGAR, 2).await.unwrap();
    let now = crate::client::filetime_now();
    let sde = c.sde.clone();
    let events = db.transaction(|tx, outbox: &mut Outbox| {
        let sde = sde.clone();
        Box::pin(async move { inject_tx(tx, outbox, &sde, f.character, None, item, 1, 1, now, false).await })
    }, |_, outbox| async move { Ok(notification_events(outbox)) }).await.unwrap_or_else(|_| panic!("injection failed"));
    assert_eq!(events.iter().filter(|event| event.as_str() == "OnItemChange").count(), 1);
    assert_eq!(events.iter().filter(|event| event.as_str() == "OnFreeSkillPointsChanged").count(), 1);
    assert_eq!(events.len(), 2);
    let events = db.transaction(|tx, outbox: &mut Outbox| {
        let sde = sde.clone();
        Box::pin(async move { convert_tx(tx, outbox, &sde, f.character, None, item, 1, 1, now, true).await })
    }, |_, outbox| async move { Ok(notification_events(outbox)) }).await.unwrap_or_else(|_| panic!("conversion failed"));
    assert_eq!(events.iter().filter(|event| event.as_str() == "OnItemChange").count(), 2);
    assert_eq!(events.len(), 2);
    fixture::cleanup(&db, f).await.unwrap();
}
