use super::*;
use crate::space::System;

#[test]
fn concord_reservation_respects_policy_without_clearing_criminal_flag() {
    let criminal = -104;
    state().pilots.entry(criminal).or_default().criminal_until = 10;

    assert!(!reserve_concord(false, 1.0, 0.5, criminal));
    assert!(!state().concord.contains(&criminal));
    assert!(is_criminal(criminal, 0));
    assert!(!reserve_concord(true, 0.4, 0.5, criminal));
    assert!(reserve_concord(true, 0.5, 0.5, criminal));
    assert!(!reserve_concord(true, 1.0, 0.5, criminal));

    let mut st = state();
    assert!(st.concord.remove(&criminal));
    st.pilots.remove(&criminal);
}

#[test]
fn discarded_concord_callback_clears_marker() {
    crate::tests::init_config();
    let criminal = -101;
    assert!(state().concord.insert(criminal));
    let callback = concord_response(PendingConcord(criminal), 1, Weak::new());
    assert!(state().concord.contains(&criminal));
    drop(callback);
    assert!(!state().concord.contains(&criminal));
}

#[test]
fn dropping_system_clears_queued_concord_marker() {
    crate::tests::init_config();
    let criminal = -102;
    assert!(state().concord.insert(criminal));
    let mut system = System::default();
    system.schedule(1, concord_response(PendingConcord(criminal), 1, Weak::new()));
    assert!(state().concord.contains(&criminal));
    drop(system);
    assert!(!state().concord.contains(&criminal));
}

#[test]
fn executed_concord_callback_allows_another_response() {
    crate::tests::init_config();
    let criminal = -103;
    assert!(state().concord.insert(criminal));
    let mut system = System::default();
    system.schedule(1, concord_response(PendingConcord(criminal), 1, Weak::new()));
    system.tick(1);
    assert!(state().concord.insert(criminal));
    drop(system);
    assert!(state().concord.remove(&criminal));
}

#[test]
fn concord_delay_uses_policy_and_clamps_security() {
    crate::tests::init_config();
    let policy = &crate::config::game().crimewatch;
    assert_eq!(concord_delay_seconds(1.0, policy.concord_base_delay_secs, policy.concord_delay_per_security_secs), 6);
    assert_eq!(concord_delay_seconds(0.9, policy.concord_base_delay_secs, policy.concord_delay_per_security_secs), 8);
    assert_eq!(concord_delay_seconds(0.1, policy.concord_base_delay_secs, policy.concord_delay_per_security_secs), 20);
    assert_eq!(concord_delay_seconds(0.75, 8, 40), 18);
}

#[test]
fn effective_crime_flag_preserves_both_timers() {
    let mut pilot = Pilot { suspect_until: 20, criminal_until: 10, ..Default::default() };
    assert_eq!(effective_flag(&pilot, 5), (2, CRIMINAL_TIMER, Some(10)));
    assert_eq!(effective_flag(&pilot, 10), (1, SUSPECT_TIMER, Some(20)));
    pilot.criminal_until = 30;
    assert_eq!(effective_flag(&pilot, 20), (2, CRIMINAL_TIMER, Some(30)));
    assert_eq!(effective_flag(&pilot, 30), (0, CRIMINAL_IDLE, None));
}

#[test]
fn suspect_is_a_legal_target_until_expiry() {
    let suspect = -105;
    state().pilots.entry(suspect).or_default().suspect_until = 20;
    assert!(is_criminal(suspect, 19));
    assert!(!is_criminal(suspect, 20));
    state().pilots.remove(&suspect);
}

#[tokio::test]
#[ignore = "requires a dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn startup_restores_active_kill_rights_without_economy_changes() {
    use crate::{dao::{kill_rights as rights, wallets::characters}, space::World};
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let now = crate::client::filetime_now();
    let account = -now;
    let owner = account + 1;
    let target = account + 2;
    let expired_target = account + 3;
    let used_target = account + 4;
    let corp = account + 5;
    let name = format!("restore-test-{now}");
    let password: Vec<u8> = Vec::new();
    db.execute("INSERT INTO game.accounts (user_id,user_name,password_hash,role) VALUES ($1,$2,$3,0)", &[&account, &name, &password]).await.unwrap();
    for (id, suffix) in [(owner, "owner"), (target, "target"), (expired_target, "expired"), (used_target, "used")] {
        let name = format!("{name}-{suffix}");
        db.execute("INSERT INTO game.characters (character_id,user_id,name,race_id,bloodline_id,gender_id,corporation_id,balance) VALUES ($1,$2,$3,1,1,1,$4,123)", &[&id, &account, &name, &corp]).await.unwrap();
    }
    let until = now + 600 * crate::consts::FILETIME_SECOND;
    let expiry = now + 1200 * crate::consts::FILETIME_SECOND;
    let mut ids = Vec::new();
    for to in [target, expired_target, used_target] {
        let right = db.transaction(|tx, _: &mut ()| Box::pin(async move {
            rights::create(tx, owner, to, now, expiry).await
        }), |value, _| async move { Ok(value) }).await.unwrap().unwrap();
        rights::activate(&db, right.kill_right_id, owner, now, until).await.unwrap().unwrap();
        ids.push(right.kill_right_id);
    }
    db.execute("UPDATE game.kill_rights SET expiry=$2 WHERE kill_right_id=$1", &[&ids[1], &now]).await.unwrap();
    db.execute("UPDATE game.kill_rights SET used_at=$2 WHERE kill_right_id=$1", &[&ids[2], &now]).await.unwrap();
    let selected: Vec<_> = rights::active_activations(&db, now).await.unwrap().into_iter().filter(|r| ids.contains(&r.kill_right_id)).map(|r| r.kill_right_id).collect();
    assert_eq!(selected, vec![ids[0]]);
    for id in [target, expired_target, used_target] {
        state().pilots.remove(&id);
    }
    let world = World::new(db.clone(), Arc::new(crate::sde::Sde::load(&db).await.unwrap()), 0);
    let before_balances = [characters::balance(&db, owner).await.unwrap(), characters::balance(&db, target).await.unwrap()];
    let before_notes = db.query("SELECT notification_id FROM game.notifications WHERE receiver_id = ANY($1)", &[&vec![owner, target, expired_target, used_target]]).await.unwrap().len();
    let restored = match crate::services::bounty::restore_activations(&db, &world).await {
        Ok(count) => count,
        Err(_) => panic!("startup restoration failed"),
    };
    assert!(restored >= 1);
    assert!(is_criminal(target, now));
    {
        let st = state();
        let pilot = st.pilots.get(&target).unwrap();
        assert_eq!(pilot.suspect_until, until);
        assert_eq!(effective_flag(pilot, now), (1, SUSPECT_TIMER, Some(until)));
        assert!(!st.pilots.contains_key(&expired_target));
        assert!(!st.pilots.contains_key(&used_target));
    }
    let criminal_until = until + crate::consts::FILETIME_SECOND;
    state().pilots.get_mut(&target).unwrap().criminal_until = criminal_until;
    let repeated = match crate::services::bounty::restore_activations(&db, &world).await {
        Ok(count) => count,
        Err(_) => panic!("repeated restoration failed"),
    };
    assert_eq!(repeated, restored);
    {
        let st = state();
        let pilot = st.pilots.get(&target).unwrap();
        assert_eq!(pilot.suspect_until, until);
        assert_eq!(effective_flag(pilot, now), (2, CRIMINAL_TIMER, Some(criminal_until)));
    }
    let right = rights::get(&db, ids[0]).await.unwrap().unwrap();
    assert_eq!((right.activated_until, right.activated_by, right.used_at), (Some(until), Some(owner), None));
    assert_eq!([characters::balance(&db, owner).await.unwrap(), characters::balance(&db, target).await.unwrap()], before_balances);
    assert_eq!(db.query("SELECT notification_id FROM game.notifications WHERE receiver_id = ANY($1)", &[&vec![owner, target, expired_target, used_target]]).await.unwrap().len(), before_notes);
    for id in [target, expired_target, used_target] {
        state().pilots.remove(&id);
    }
    db.execute("DELETE FROM game.kill_rights WHERE kill_right_id = ANY($1)", &[&ids]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE user_id=$1", &[&account]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await.unwrap();
}
