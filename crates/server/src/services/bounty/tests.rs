//! Bounty wire, validation and allocation behavior.
use super::*;
use crate::dao::bounties::Bounty;
use marshal::Value;

#[test]
fn placement_amounts_reject_nonfinite_and_apply_owner_minimums() {
    for kind in [bounties::CHARACTER, bounties::CORPORATION, bounties::ALLIANCE] {
        assert!(!bounties::valid_amount(kind, f64::NAN));
        assert!(!bounties::valid_amount(kind, f64::INFINITY));
        assert!(!bounties::valid_amount(kind, -1.0));
        assert!(!bounties::valid_amount(kind, MAX_BOUNTY + 1.0));
        assert!(bounties::valid_amount(kind, MAX_BOUNTY));
    }
    assert!(bounties::valid_amount(bounties::CHARACTER, MIN_CHARACTER_BOUNTY));
    assert!(!bounties::valid_amount(bounties::CORPORATION, MIN_CORPORATION_BOUNTY - 1.0));
    assert!(!bounties::valid_amount(bounties::ALLIANCE, MIN_ALLIANCE_BOUNTY - 1.0));
}

#[test]
fn sale_price_rejects_unrepresentable_values() {
    assert!(kill_rights::sale_price(f64::NAN).is_none());
    assert!(kill_rights::sale_price(f64::INFINITY).is_none());
    assert!(kill_rights::sale_price(f64::MAX).is_none());
    assert!(kill_rights::sale_price(1e34).is_none());
    assert_eq!(kill_rights::sale_price(0.0), Some(0.0));
}

#[test]
fn payout_apportionment_conserves_cents_and_breaks_ties_by_owner_id() {
    let shares = payout::shares(&[(1, 300_000_000), (2, 700_000_000)], 400_000_000);
    assert_eq!(shares, vec![120_000_000, 280_000_000]);
    assert_eq!(shares.iter().sum::<i64>(), 400_000_000);
    assert_eq!(payout::shares(&[(20, 1), (10, 1), (30, 1)], 2), vec![1, 1, 0]);
    assert_eq!(payout::shares(&[(1, 0)], 0), vec![0]);
}

#[test]
fn bounty_wire_row_identifies_unknown_and_affiliations() {
    let bounty = Bounty { target_id: 42, amount: 0.0, corporation_id: None, alliance_id: None };
    let Value::Instance(_, state) = bounties::row(&bounty) else { panic!("expected KeyVal"); };
    assert_eq!(state.get("targetID"), Some(&Value::Int(42)));
    assert_eq!(state.get("bounty"), Some(&Value::Float(0.0)));
    assert_eq!(state.get("corporationID"), Some(&Value::None));
    assert_eq!(state.get("allianceID"), Some(&Value::None));
}

#[test]
fn bounty_pair_wire_shape_keeps_requested_owner_id() {
    let bounty = Bounty { target_id: 42, amount: 0.0, corporation_id: None, alliance_id: None };
    let pairs = bounties::pairs(&[bounty]);
    let [pair] = pairs.items().unwrap() else { panic!("expected one pair"); };
    let [id, Value::Instance(_, fields)] = pair.items().unwrap() else { panic!("expected ID and KeyVal"); };
    assert_eq!(*id, Value::Int(42));
    assert_eq!(fields.get("targetID"), Some(id));
}


#[tokio::test]
#[ignore = "requires a dedicated INFINITY_TEST_DATABASE_URL"]
async fn right_transactions_guard_purchase_restore_consume_and_expiry() {
    use crate::dao::kill_rights as rights;
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let now = crate::client::filetime_now();
    let owner = -now;
    let unknown = crate::dao::bounties::get(&db, &[owner]).await.unwrap();
    assert_eq!(unknown.len(), 1);
    assert_eq!(unknown[0].target_id, owner);
    assert_eq!(unknown[0].amount, 0.0);
    assert_eq!(crate::dao::bounties::payout_limit(&db, 100, 0.35, 0.2).await.unwrap(), 7);
    let target = owner + 1;
    let buyer = owner + 2;
    let id = db.transaction(|tx, _: &mut ()| Box::pin(async move {
        let right = rights::create(tx, owner, target, now, now + 1000).await?.unwrap();
        assert!(rights::create(tx, owner, target, now, now + 1000).await?.is_none());
        Ok::<_, crate::db::Error>(right.kill_right_id)
    }), |value, _| async move { Ok(value) }).await.unwrap();

    let cancelled = rights::cancel(&db, id, owner, target, now).await.unwrap().unwrap();
    assert!(cancelled.price.is_none());
    rights::sell(&db, id, owner, 0.0, Some(buyer), now).await.unwrap().unwrap();
    assert!(rights::visible(&db, &[target], &[buyer], now).await.unwrap().iter().any(|r| r.kill_right_id == id));
    assert!(rights::visible(&db, &[target], &[target], now).await.unwrap().is_empty());
    assert!(rights::buy(&db, id, 0.0, target, &[target], now, now + 100, true).await.unwrap().is_none());
    let viewers = [buyer];
    let (a, b) = tokio::join!(
        rights::buy(&db, id, 0.0, buyer, &viewers, now, now + 100, false),
        rights::buy(&db, id, 0.0, buyer, &viewers, now, now + 100, false)
    );
    assert_ne!(a.unwrap().is_some(), b.unwrap().is_some());
    let restored = rights::activate(&db, id, buyer, now + 1, now + 200).await.unwrap().unwrap();
    assert_eq!(restored.activated_until, Some(now + 100));
    let consumed = db.transaction(|tx, _: &mut ()| Box::pin(async move {
        rights::consume_active(tx, target, now + 2).await
    }), |value, _| async move { Ok(value) }).await.unwrap();
    assert_eq!(consumed.len(), 1);
    assert!(rights::consume_active(&db, target, now + 3).await.unwrap().is_empty());

    let buyback = db.transaction(|tx, _: &mut ()| Box::pin(async move {
        rights::create(tx, owner, target, now, now + 1000).await
    }), |value, _| async move { Ok(value) }).await.unwrap().unwrap();
    for restriction in [owner + 3, owner + 4] {
        rights::sell(&db, buyback.kill_right_id, owner, 0.0, Some(restriction), now).await.unwrap().unwrap();
        assert_eq!(rights::visible(&db, &[target], &[buyer, restriction], now).await.unwrap().len(), 1);
        assert!(rights::visible(&db, &[target], &[buyer], now).await.unwrap().is_empty());
    }
    rights::sell(&db, buyback.kill_right_id, owner, 0.0, None, now).await.unwrap().unwrap();
    let bought = rights::buy(&db, buyback.kill_right_id, 0.0, target, &[target], now, now + 100, true).await.unwrap().unwrap();
    assert_eq!(bought.used_at, Some(now));
    assert!(rights::activate(&db, buyback.kill_right_id, owner, now, now + 100).await.unwrap().is_none());

    let expired = db.transaction(|tx, _: &mut ()| Box::pin(async move {
        rights::create(tx, owner, target, now, now + 5).await
    }), |value, _| async move { Ok(value) }).await.unwrap().unwrap();
    assert!(rights::delete_expired(&db, expired.kill_right_id, now + 5).await.unwrap());
}

#[tokio::test]
#[ignore = "requires a dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn payout_handler_commits_wallet_pool_notifications_and_replay() {
    use crate::{dao::bounties as dao, scheduler::Job, space::World};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let now = crate::client::filetime_now();
    let account = -now;
    let victim = account + 1;
    let hunter = account + 2;
    let corp = account + 3;
    let name = format!("bounty-test-{now}");
    let password: Vec<u8> = Vec::new();
    db.execute("INSERT INTO game.accounts (user_id,user_name,password_hash,role) VALUES ($1,$2,$3,0)", &[&account, &name, &password]).await.unwrap();
    for (id, suffix) in [(victim, "victim"), (hunter, "hunter")] {
        let name = format!("{name}-{suffix}");
        db.execute("INSERT INTO game.characters (character_id,user_id,name,race_id,bloodline_id,gender_id,corporation_id,balance) VALUES ($1,$2,$3,1,1,1,$4,0)", &[&id, &account, &name, &corp]).await.unwrap();
    }
    dao::add(&db, victim, bounties::CHARACTER, 10_000_000.0, MAX_BOUNTY).await.unwrap();
    dao::contribute(&db, victim, hunter, 10_000_000.0, now).await.unwrap();
    let world = World::new(db.clone(), std::sync::Arc::new(crate::sde::Sde::load(&db).await.unwrap()), 0);
    let (payout_tx, mut payout_rx) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(hunter, payout_tx);
    let ship_type = -((now % i64::from(i32::MAX)) as i32) - 1;
    let inserted = db.execute("INSERT INTO game.retail_prices (type_id,average,adjusted) VALUES ($1,$2,$2) ON CONFLICT DO NOTHING", &[&ship_type, &20_000_000.0_f64]).await.unwrap();
    assert_eq!(inserted, 1, "isolated price fixture must not overwrite an existing price");
    let ship = account + 4;
    crate::services::crimewatch::note_damage(ship, hunter, 100.0);
    assert!(crate::services::crimewatch::ship_lost(&db, &world, 0, victim, corp, ship, ship_type, &vec![]).await.is_ok());
    db.execute("DELETE FROM game.retail_prices WHERE type_id=$1", &[&ship_type]).await.unwrap();
    let kill = db.query_one("SELECT kill_id,isk_lost,final_char_id,victim_ship_type FROM game.killmails WHERE victim_id=$1 ORDER BY kill_id DESC LIMIT 1", &[&victim]).await.unwrap();
    let kill_id: i64 = kill.get("kill_id");
    assert_eq!(kill.get::<_, f64>("isk_lost"), 20_000_000.0);
    assert_eq!(kill.get::<_, Option<i64>>("final_char_id"), Some(hunter));
    assert_eq!(kill.get::<_, i32>("victim_ship_type"), ship_type);
    let job_id = format!("{}:{kill_id}", payout::KIND);
    let row = db.query_one("SELECT job_id,kind,due_at,payload FROM game.jobs WHERE job_id=$1", &[&job_id]).await.unwrap();
    let job = Job { id: row.get("job_id"), kind: row.get("kind"), due_at: row.get("due_at"), payload: row.get("payload") };
    let payload: payout::Payload = serde_json::from_str(&job.payload).unwrap();
    assert_eq!((payload.killmail_id, payload.victim, payload.victim_corp, payload.ship_type, payload.final_char), (kill_id, victim, corp, ship_type, hunter));
    assert_eq!(payload.isk_lost, 20_000_000.0);
    payout::handle(db.clone(), world.clone(), job.clone()).await.unwrap();
    payout::handle(db.clone(), world.clone(), job).await.unwrap();
    let events: Vec<_> = std::iter::from_fn(|| payout_rx.try_recv().ok()).collect();
    let payouts: Vec<_> = events.iter().filter(|(name, _)| name == "OnBountyAddedToPayout").collect();
    assert_eq!(payouts.len(), 1);
    let [dict] = payouts[0].1.as_slice() else { panic!("expected payout dict"); };
    assert_eq!(dict.get("amount"), Some(&Value::Float(4_000_000.0)));
    assert!(dict.get("payoutTime").and_then(Value::as_int).is_some());
    assert_eq!(dict.get("enemyTypeID"), Some(&Value::Int(i64::from(ship_type))));
    assert_eq!(dict.get("isModified"), Some(&Value::Bool(false)));
    assert_eq!(dao::get(&db, &[victim]).await.unwrap()[0].amount, 6_000_000.0);
    assert_eq!(crate::dao::wallets::characters::balance(&db, hunter).await.unwrap(), 4_000_000.0);
    let notices = db.query("SELECT data FROM game.notifications WHERE receiver_id=$1 AND type_id=111", &[&hunter]).await.unwrap();
    assert_eq!(notices.len(), 1);
    let data: String = notices[0].get("data");
    assert!(data.contains("victimID") && data.contains("bounty"));
    assert!(kill_rights::earn(&db, &world, victim, hunter).await.is_ok());
    assert!(kill_rights::earn(&db, &world, victim, hunter).await.is_ok());
    let rights = crate::dao::kill_rights::mine(&db, victim, now).await.unwrap();
    assert_eq!(rights.len(), 1);
    let id = rights[0].kill_right_id;
    let active = crate::dao::kill_rights::activate(&db, id, victim, now, now + crate::consts::FILETIME_SECOND).await.unwrap();
    assert!(active.is_some());
    let consumed = db.transaction(|tx, outbox: &mut crate::db::outbox::Outbox| Box::pin(async move {
        kill_rights::consume(tx, outbox, hunter, now).await
    }), |value, outbox| async move { crate::db::outbox::dispatch_world(value, outbox, &world) }).await;
    assert!(consumed.is_ok());
    let notices = db.query("SELECT type_id,data FROM game.notifications WHERE receiver_id=$1 AND type_id IN (117,118) ORDER BY type_id", &[&victim]).await.unwrap();
    assert_eq!(notices.len(), 2);
    let created: String = notices[0].get("data");
    let used: String = notices[1].get("data");
    assert!(created.contains("charID") && used.contains("charID"));
}

async fn rpc(c: &mut Client, service: &str, method: &str, args: &[Value]) -> CallResult {
    let kwargs = Value::Dict(vec![]);
    let call = Call { service, method, args, kwargs: &kwargs, bound: false, bind_params: None };
    dispatch(c, &call).await.expect("bounty method must be routed")
}

#[tokio::test]
#[ignore = "requires a dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn service_placement_sale_purchase_and_consumption() {
    use crate::{conn::Conn, dao::{bounties as pools, kill_rights as rights, wallets::characters}, handshake::Login, space::World};
    use std::sync::Arc;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let now = crate::client::filetime_now();
    let account = -now;
    let target = account + 1;
    let owner = account + 2;
    let buyer = account + 3;
    let corp = account + 4;
    let name = format!("bounty-service-{now}");
    let password: Vec<u8> = Vec::new();
    db.execute("INSERT INTO game.accounts (user_id,user_name,password_hash,role) VALUES ($1,$2,$3,0)", &[&account, &name, &password]).await.unwrap();
    for (id, suffix, balance) in [(target, "target", 0.0), (owner, "owner", 3_000_000.0), (buyer, "buyer", 500.0)] {
        let name = format!("{name}-{suffix}");
        db.execute("INSERT INTO game.characters (character_id,user_id,name,race_id,bloodline_id,gender_id,corporation_id,balance) VALUES ($1,$2,$3,1,1,1,$4,$5::float8::numeric)", &[&id, &account, &name, &corp, &balance]).await.unwrap();
    }
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let world = World::new(db.clone(), sde.clone(), 0);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let (stream, address) = listener.accept().await.unwrap();
    let mut client = Client::new(Conn::new(stream, address), Arc::new(crate::config::Config::default()), db.clone(), sde,
        crate::chat::Hub::new("localhost"), world.clone(),
        Login { user_id: account, user_name: name, role: 0, client_id: 1, session_id: 1, language_id: "EN".into() });
    let _peer = peer;
    let (owner_tx, mut owner_rx) = tokio::sync::mpsc::unbounded_channel();
    let (target_tx, mut target_rx) = tokio::sync::mpsc::unbounded_channel();
    let (buyer_tx, mut buyer_rx) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(owner, owner_tx);
    world.register_pilot(target, target_tx);
    world.register_pilot(buyer, buyer_tx);
    client.session.apply(vec![("charid", Value::Int(owner)), ("corpid", Value::Int(corp))]);

    let unknown = account + 100;
    let result = rpc(&mut client, "bountyProxy", "GetBounties", &[Value::list([Value::Int(unknown)])]).await;
    let pairs = match result { Ok(value) => value, Err(_) => panic!("unknown bounty query failed") };
    let [pair] = pairs.items().unwrap() else { panic!("expected one bounty pair"); };
    let [id, Value::Instance(_, fields)] = pair.items().unwrap() else { panic!("expected bounty pair"); };
    assert_eq!(*id, Value::Int(unknown));
    assert_eq!(fields.get("bounty"), Some(&Value::Float(0.0)));
    assert!(rpc(&mut client, "bountyProxy", "AddToBounty", &[Value::Int(target), Value::Float(1_000_000.0)]).await.is_ok());
    assert_eq!(characters::balance(&db, owner).await.unwrap(), 2_000_000.0);
    assert_eq!(pools::get(&db, &[target]).await.unwrap()[0].amount, 1_000_000.0);
    let notices = db.query("SELECT data FROM game.notifications WHERE receiver_id=$1 AND type_id=112", &[&target]).await.unwrap();
    assert_eq!(notices.len(), 1);
    let data: String = notices[0].get("data");
    assert!(data.contains("bountyPlacerID") && data.contains("bounty"));
    assert!(rpc(&mut client, "bountyProxy", "AddToBounty", &[Value::Int(target), Value::Float(3_000_000.0)]).await.is_err());
    assert_eq!(characters::balance(&db, owner).await.unwrap(), 2_000_000.0);
    assert_eq!(pools::get(&db, &[target]).await.unwrap()[0].amount, 1_000_000.0);
    assert_eq!(db.query("SELECT data FROM game.notifications WHERE receiver_id=$1 AND type_id=112", &[&target]).await.unwrap().len(), 1);

    assert!(kill_rights::earn(&db, &world, owner, target).await.is_ok());
    assert!(kill_rights::earn(&db, &world, owner, target).await.is_ok());
    let right = rights::mine(&db, owner, now).await.unwrap();
    assert_eq!(right.len(), 1);
    let id = right[0].kill_right_id;
    assert_eq!(rights::mine(&db, target, now).await.unwrap().len(), 1);
    for character in [owner, target] {
        client.session.apply(vec![("charid", Value::Int(character))]);
        let mine = match rpc(&mut client, "bountyProxy", "GetMyKillRights", &[]).await {
            Ok(value) => value, Err(_) => panic!("right query failed"),
        };
        assert_eq!(mine.items().unwrap().len(), 1);
    }
    client.session.apply(vec![("charid", Value::Int(owner))]);
    assert_eq!(db.query("SELECT data FROM game.notifications WHERE receiver_id=$1 AND type_id=117", &[&owner]).await.unwrap().len(), 1);
    assert!(rpc(&mut client, "bountyProxy", "SellKillRight", &[Value::Int(id), Value::Float(600.0), Value::Int(buyer)]).await.is_ok());
    let listed = rights::get(&db, id).await.unwrap().unwrap();
    assert_eq!((listed.price, listed.restricted_to), (Some(600.0), Some(buyer)));
    let notices = db.query("SELECT data FROM game.notifications WHERE receiver_id=$1 AND type_id=115", &[&target]).await.unwrap();
    assert_eq!(notices.len(), 1);
    let data: String = notices[0].get("data");
    assert!(data.contains("charID") && data.contains("price") && data.contains("toEntityID"));
    client.session.apply(vec![("charid", Value::Int(buyer))]);
    assert!(rpc(&mut client, "killRightMgr", "BuyKillRight", &[Value::Int(id), Value::Float(600.0)]).await.is_err());
    assert_eq!(characters::balance(&db, owner).await.unwrap(), 2_000_000.0);
    assert_eq!(characters::balance(&db, buyer).await.unwrap(), 500.0);
    assert_eq!(rights::get(&db, id).await.unwrap().unwrap().price, Some(600.0));
    client.session.apply(vec![("charid", Value::Int(owner))]);
    assert!(rpc(&mut client, "bountyProxy", "SellKillRight", &[Value::Int(id), Value::Float(250.0), Value::Int(buyer)]).await.is_ok());
    client.session.apply(vec![("charid", Value::Int(buyer))]);
    assert!(rpc(&mut client, "killRightMgr", "BuyKillRight", &[Value::Int(id), Value::Float(250.0)]).await.is_ok());
    assert_eq!(characters::balance(&db, owner).await.unwrap(), 2_000_250.0);
    assert_eq!(characters::balance(&db, buyer).await.unwrap(), 250.0);
    let active = rights::get(&db, id).await.unwrap().unwrap();
    assert_eq!(active.activated_by, Some(buyer));
    assert!(active.activated_until.is_some());
    assert!(rpc(&mut client, "killRightMgr", "ActivateKillRight", &[Value::Int(id)]).await.is_ok());
    assert_eq!(rights::get(&db, id).await.unwrap().unwrap().activated_until, active.activated_until);
    let consumed = db.transaction(|tx, outbox: &mut crate::db::outbox::Outbox| Box::pin(async move {
        kill_rights::consume(tx, outbox, target, crate::client::filetime_now()).await
    }), |value, outbox| async move { crate::db::outbox::dispatch_world(value, outbox, &world) }).await;
    assert!(consumed.is_ok());
    for (index, rx) in [&mut owner_rx, &mut target_rx, &mut buyer_rx].into_iter().enumerate() {
        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        let used: Vec<_> = events.iter().filter(|(name, _)| name == "OnKillRightUsed").collect();
        assert_eq!(used.len(), 1);
        assert_eq!(used[0].1, vec![Value::Int(id), Value::Int(target)]);
        if index == 0 {
            assert!(events.iter().any(|(name, args)| name == "OnKillRightForYouSold" && *args == vec![Value::Int(id)]));
        }
        if index == 1 {
            assert!(events.iter().any(|(name, args)| name == "OnCriminalTimerUpdate"
                && args == &vec![Value::Int(304), Value::Int(active.activated_until.unwrap())]));
        }
    }

    let alliance = account + 5;
    let other_corp = account + 6;
    let org_name = format!("bounty-org-{now}");
    let other_name = format!("bounty-other-{now}");
    let alliance_name = format!("bounty-alliance-{now}");
    db.execute("INSERT INTO game.alliances (alliance_id,name,short_name,creator_corp_id,creator_char_id,created) VALUES ($1,$2,$2,$3,$4,$5)",
        &[&alliance, &alliance_name, &corp, &owner, &now]).await.unwrap();
    for (id, name) in [(corp, &org_name), (other_corp, &other_name)] {
        db.execute("INSERT INTO game.corporations (corporation_id,name,ticker,creator_id,station_id,created,alliance_id) VALUES ($1,$2,$2,$3,0,$4,$5)",
            &[&id, name, &owner, &now, &alliance]).await.unwrap();
    }
    db.execute("UPDATE game.characters SET corporation_id=$1 WHERE character_id=$2", &[&other_corp, &buyer]).await.unwrap();
    db.execute("UPDATE game.characters SET balance=$1::float8::numeric WHERE character_id=$2", &[&200_000_000.0_f64, &owner]).await.unwrap();
    client.session.apply(vec![("charid", Value::Int(owner)), ("corpid", Value::Int(corp)), ("allianceid", Value::Int(alliance))]);
    for (id, amount) in [(corp, 20_000_000.0), (alliance, 100_000_000.0)] {
        assert!(rpc(&mut client, "bountyProxy", "AddToBounty", &[Value::Int(id), Value::Float(amount)]).await.is_ok());
    }
    assert_eq!(characters::balance(&db, owner).await.unwrap(), 80_000_000.0);
    for (type_id, expected) in [(113_i32, vec![target, owner]), (114_i32, vec![target, owner, buyer])] {
        let rows = db.query("SELECT receiver_id,data FROM game.notifications WHERE type_id=$1 AND receiver_id=ANY($2) ORDER BY receiver_id",
            &[&type_id, &vec![target, owner, buyer]]).await.unwrap();
        let recipients: Vec<i64> = rows.iter().map(|row| row.get("receiver_id")).collect();
        assert_eq!(recipients, expected);
        for row in rows {
            let data: String = row.get("data");
            assert!(data.contains("bountyPlacerID") && data.contains("bounty"));
        }
    }
    let affiliations = pools::get(&db, &[target, corp, alliance]).await.unwrap();
    assert_eq!((affiliations[0].corporation_id, affiliations[0].alliance_id), (Some(corp), Some(alliance)));
    assert_eq!((affiliations[1].corporation_id, affiliations[1].alliance_id), (Some(corp), Some(alliance)));
    assert_eq!((affiliations[2].corporation_id, affiliations[2].alliance_id), (None, Some(alliance)));
    let contributions = match rpc(&mut client, "bountyProxy", "GetMyBounties", &[]).await {
        Ok(value) => value, Err(_) => panic!("contribution query failed"),
    };
    assert_eq!(contributions.items().unwrap().len(), 3);
    let combined = match rpc(&mut client, "bountyProxy", "GetBountiesAndKillRights",
        &[Value::list([Value::Int(target), Value::Int(corp), Value::Int(alliance)]), Value::list([Value::Int(target)])]).await {
        Ok(value) => value, Err(_) => panic!("combined query failed"),
    };
    let [bounty_pairs, rights] = combined.items().unwrap() else { panic!("expected combined response"); };
    assert_eq!(bounty_pairs.items().unwrap().len(), 3);
    assert!(rights.items().unwrap().is_empty());
    for (top, search, id) in [
        ("GetTopPilotBounties", "SearchCharBounties", target),
        ("GetTopCorpBounties", "SearchCorpBounties", corp),
        ("GetTopAllianceBounties", "SearchAllianceBounties", alliance),
    ] {
        let top = match rpc(&mut client, "bountyProxy", top, &[Value::Int(0)]).await {
            Ok(value) => value, Err(_) => panic!("bounty leaderboard query failed"),
        };
        let [rows, _time] = top.items().unwrap() else { panic!("expected leaderboard and time"); };
        assert!(!rows.items().unwrap().is_empty());
        let search = match rpc(&mut client, "bountyProxy", search, &[Value::Int(id)]).await {
            Ok(value) => value, Err(_) => panic!("bounty search failed"),
        };
        assert!(search.items().unwrap().iter().any(|pair| {
            let Some([_, Value::Instance(_, fields)]) = pair.items() else { return false; };
            fields.get("targetID") == Some(&Value::Int(id))
        }));
    }
    assert!(pools::record_payout(&db, account + 7, owner, 400_000_000, now).await.unwrap());
    for method in ["GetTopPilotBountyHunters", "GetTopCorporationBountyHunters", "GetTopAllianceBountyHunters"] {
        let top = match rpc(&mut client, "bountyProxy", method, &[Value::Int(0)]).await {
            Ok(value) => value, Err(_) => panic!("hunter leaderboard query failed"),
        };
        let [rows] = top.items().unwrap() else { panic!("expected hunter leaderboard"); };
        assert!(!rows.items().unwrap().is_empty());
    }
}
