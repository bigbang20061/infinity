use super::super::*;
use super::fixtures::*;
use super::fixtures::activate;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn booster_activation_persists_timing_selection_and_wire_record() {
    let _market = crate::services::market::lock().await;
    let (db, f, c, _peer) = setup().await;
    let ty = booster(&c.sde);
    let source = item(&db, f.character, ty, STATION, 1).await;
    let now = filetime_now();
    let draws = candidates(&c.sde, ty).into_iter().map(|(id, _)| (id, 0.5)).collect();
    let before = fingerprint(&db, f.character).await;
    assert!(activate(&db, &c.sde, f.character, source, Some(STATION + 1), now, true, vec![]).await.is_err());
    assert_eq!(fingerprint(&db, f.character).await, before);
    let (_, effects) = activate(&db, &c.sde, f.character, source, Some(STATION), now, true, draws).await.unwrap_or_else(|_| panic!("booster activation failed"));
    assert_eq!(notices(&effects, crate::client::BRAIN_REFRESH), 1);
    assert_eq!(notices(&effects, "OnServerBoostersChanged"), 1);
    let rows = brain::list(&db, f.character).await.unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.item_id, source);
    assert_eq!(row.activated_at, Some(now));
    assert!(row.expires_at.unwrap() > now);
    let ids = row.effect_ids.as_ref().expect("selected effects metadata");
    assert!(ids.windows(2).all(|p| p[0] < p[1]));
    let wire = super::super::super::brain_reads::get_boosters(&c, f.character).await.unwrap_or_else(|_| panic!("booster read failed"));
    let Value::Dict(entries) = wire else { panic!("booster read must be a dictionary") };
    assert_eq!(entries.len(), 1);
    assert!(entries[0].0 == Value::Int(source));
    assert_eq!(entries[0].1, crate::services::keyval([
        ("itemID", Value::Int(source)),
        ("boosterTypeID", Value::Int(i64::from(ty))),
        ("expiryTime", Value::Int(row.expires_at.unwrap())),
        ("boosterDuration", Value::Int(ms_from_filetime_ticks(row.expires_at.unwrap() - now))),
        ("sideEffectIDs", Value::list(ids.iter().map(|id| Value::Int(i64::from(*id))))),
    ]));
    assert!(crate::dao::items::fit::brain_items(&db, f.character, row.expires_at.unwrap()).await.unwrap().is_empty());
    db.execute("UPDATE game.items SET brain_expires_at=$2 WHERE item_id=$1", &[&source, &(now - 1)]).await.unwrap();
    let expired = super::super::super::brain_reads::get_boosters(&c, f.character).await.unwrap_or_else(|_| panic!("expired booster read failed"));
    assert!(matches!(expired, Value::Dict(entries) if entries.is_empty()));
    assert!(brain::get(&db, f.character, source).await.unwrap().is_some());
    cleanup(&db, f).await;
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn explicit_zero_credit_survives_stack_split() {
    let _market = crate::services::market::lock().await;
    let (db, f, c, _peer) = setup().await;
    let ty = c.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_IMPLANT) && t.group_id == Some(GROUP_BOOSTER)
        && t.attribute(ATTR_BOOSTERNESS).is_some_and(|v| v > 0.0 && v.fract() == 0.0)
        && t.attribute(ATTR_BOOSTER_DURATION).is_some_and(|v| v > 0.0 && v.is_finite())
        && t.attribute(ATTR_NON_DIMINISHING_SKILL_INJECTOR_USES).is_some_and(|v| v > 0.0 && v.fract() == 0.0)).expect("credit booster required").type_id;
    for (skill, level) in requirements(&c.sde, ty) {
        crate::dao::skills::training::starting_skill(&db, f.character, skill, level, 0).await.unwrap();
    }
    let source = item(&db, f.character, ty, STATION, 2).await;
    db.execute("INSERT INTO game.item_attributes (item_id,attribute_id,value) VALUES ($1,$2,0)", &[&source, &ATTR_NON_DIMINISHING_SKILL_INJECTOR_USES]).await.unwrap();
    let mut now = filetime_now();
    if let Some(deadline) = c.sde.types.attribute(ty, ATTR_BOOSTER_LAST_INJECTION_DATETIME) {
        now = now.min(filetime_from_dogma_days(deadline).expect("valid deadline"));
    }
    if let Some(hours) = c.sde.types.attribute(ty, ATTR_BOOSTER_MAX_CHAR_AGE_HOURS) {
        let created = crate::dao::characters::social::public_info(&db, f.character, FILETIME_SECOND, FILETIME_UNIX_EPOCH).await.unwrap().unwrap().created;
        now = now.min(created + filetime_ticks_from_hours(hours).expect("valid age"));
    }
    let draws = candidates(&c.sde, ty).into_iter().map(|(id, _)| (id, 0.5)).collect();
    let (_, effects) = activate(&db, &c.sde, f.character, source, Some(STATION), now, true, draws).await.unwrap_or_else(|_| panic!("credit booster activation failed"));
    let installed = brain::list(&db, f.character).await.unwrap();
    assert_eq!(installed.len(), 1);
    assert_ne!(installed[0].item_id, source);
    assert_eq!(dogma::overrides(&db, installed[0].item_id).await.unwrap().iter().find(|a| a.attribute_id == ATTR_NON_DIMINISHING_SKILL_INJECTOR_USES).map(|a| a.value), Some(0.0));
    assert_eq!(available_credits(&db, &c.sde, f.character, now).await.unwrap_or_else(|_| panic!("credit read failed")), 0);
    assert_eq!(notices(&effects, "OnNonDiminishingInjectionsAdded"), 0);
    db.execute("DELETE FROM game.items WHERE item_id=$1", &[&installed[0].item_id]).await.unwrap();
    let grant = item(&db, f.character, ty, STATION, 1).await;
    let draws = candidates(&c.sde, ty).into_iter().map(|(id, _)| (id, 0.5)).collect();
    let (_, effects) = activate(&db, &c.sde, f.character, grant, Some(STATION), now, true, draws).await.unwrap_or_else(|_| panic!("credit grant activation failed"));
    let initial = c.sde.types.attribute(ty, ATTR_NON_DIMINISHING_SKILL_INJECTOR_USES).unwrap() as i64;
    assert_eq!(available_credits(&db, &c.sde, f.character, now).await.unwrap_or_else(|_| panic!("credit grant read failed")), initial);
    assert_eq!(notices(&effects, "OnNonDiminishingInjectionsAdded"), 1);
    assert!(dogma::set_override(&db, grant, ATTR_NON_DIMINISHING_SKILL_INJECTOR_USES, 0.0).await.unwrap());
    assert_eq!(available_credits(&db, &c.sde, f.character, now).await.unwrap_or_else(|_| panic!("remaining credit read failed")), 0);
    let expiry = brain::get(&db, f.character, grant).await.unwrap().unwrap().expires_at.unwrap();
    assert_eq!(available_credits(&db, &c.sde, f.character, expiry).await.unwrap_or_else(|_| panic!("expired credit read failed")), 0);
    cleanup(&db, f).await;
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn static_age_and_deadline_refuse_after_cutoff_atomically() {
    let _market = crate::services::market::lock().await;
    let (db, f, c, _peer) = setup().await;
    let created = crate::dao::characters::social::public_info(&db, f.character, FILETIME_SECOND, FILETIME_UNIX_EPOCH).await.unwrap().unwrap().created;
    for attribute in [ATTR_BOOSTER_MAX_CHAR_AGE_HOURS, ATTR_BOOSTER_LAST_INJECTION_DATETIME] {
        let other = if attribute == ATTR_BOOSTER_MAX_CHAR_AGE_HOURS { ATTR_BOOSTER_LAST_INJECTION_DATETIME } else { ATTR_BOOSTER_MAX_CHAR_AGE_HOURS };
        let ty = c.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_IMPLANT) && t.group_id == Some(GROUP_BOOSTER)
            && t.attribute(ATTR_BOOSTERNESS).is_some_and(|v| v > 0.0 && v.fract() == 0.0)
            && t.attribute(ATTR_BOOSTER_DURATION).is_some_and(|v| v.is_finite() && v > 0.0)
            && t.attribute(attribute).is_some() && t.attribute(other).is_none()).expect("booster with isolated eligibility cutoff required").type_id;
        for (skill, level) in requirements(&c.sde, ty) {
            db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,$3,0) ON CONFLICT (character_id,type_id) DO UPDATE SET level=GREATEST(game.skills.level,EXCLUDED.level)", &[&f.character, &skill, &level]).await.unwrap();
        }
        let raw = c.sde.types.attribute(ty, attribute).unwrap();
        let cutoff = if attribute == ATTR_BOOSTER_MAX_CHAR_AGE_HOURS {
            created.checked_add(filetime_ticks_from_hours(raw).expect("valid age")).unwrap()
        } else { filetime_from_dogma_days(raw).expect("valid date") };
        let source = item(&db, f.character, ty, STATION, 1).await;
        let before = fingerprint(&db, f.character).await;
        assert!(activate(&db, &c.sde, f.character, source, Some(STATION), cutoff + 1, true, vec![]).await.is_err());
        assert_eq!(fingerprint(&db, f.character).await, before);
        db.execute("DELETE FROM game.items WHERE item_id=$1", &[&source]).await.unwrap();
    }
    cleanup(&db, f).await;
}
