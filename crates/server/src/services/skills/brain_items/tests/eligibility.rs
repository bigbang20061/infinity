use super::super::*;
use super::fixtures::*;
use super::fixtures::activate;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn eligibility_cutoffs_are_inclusive() {
    let _market = crate::services::market::lock().await;
    let (db, f, c, _peer) = setup().await;
    let created = crate::dao::characters::social::public_info(&db, f.character, FILETIME_SECOND, FILETIME_UNIX_EPOCH).await.unwrap().unwrap().created;
    for attribute in [ATTR_BOOSTER_MAX_CHAR_AGE_HOURS, ATTR_BOOSTER_LAST_INJECTION_DATETIME] {
        let other = if attribute == ATTR_BOOSTER_MAX_CHAR_AGE_HOURS { ATTR_BOOSTER_LAST_INJECTION_DATETIME } else { ATTR_BOOSTER_MAX_CHAR_AGE_HOURS };
        let ty = c.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_IMPLANT) && t.group_id == Some(GROUP_BOOSTER)
            && t.attribute(ATTR_BOOSTERNESS).is_some_and(|v| v > 0.0 && v.fract() == 0.0)
            && t.attribute(ATTR_BOOSTER_DURATION).is_some_and(|v| v.is_finite() && v > 0.0)
            && t.attribute(attribute).is_some() && t.attribute(other).is_none()).expect("published booster with isolated cutoff required").type_id;
        for (skill, level) in requirements(&c.sde, ty) {
            db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,$3,0) ON CONFLICT (character_id,type_id) DO UPDATE SET level=GREATEST(game.skills.level,EXCLUDED.level)", &[&f.character, &skill, &level]).await.unwrap();
        }
        let raw = c.sde.types.attribute(ty, attribute).unwrap();
        let cutoff = if attribute == ATTR_BOOSTER_MAX_CHAR_AGE_HOURS { created.checked_add(filetime_ticks_from_hours(raw).expect("valid hours")).unwrap() } else { filetime_from_dogma_days(raw).expect("valid SDE days") };
        let source = item(&db, f.character, ty, STATION, 1).await;
        let draws = candidates(&c.sde, ty).into_iter().map(|(id, _)| (id, 0.5)).collect();
        activate(&db, &c.sde, f.character, source, Some(STATION), cutoff, true, draws).await.unwrap_or_else(|_| panic!("booster refused at inclusive eligibility cutoff"));
        db.execute("DELETE FROM game.items WHERE item_id=$1", &[&source]).await.unwrap();
        crate::scheduler::cancel(&db, &format!("{}:{}", completion::KIND, f.character)).await.unwrap();
    }
    cleanup(&db, f).await;
}
