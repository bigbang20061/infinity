//! Group authorization, exclusions and audit changes through the client service.
use super::*;
use crate::services::access_groups::{self, ADMIN, EXCLUDED, MANAGER, MEMBER, NONE, PUBLIC};
use crate::services::tests::fixture;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn group_permissions_and_audit_are_durable() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let manager = fixture::create(&db).await.unwrap();
    let member = fixture::create(&db).await.unwrap();
    let outsider = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut a, _a) = crate::services::tests::client(test_db.clone(), owner.character).await;
        let (mut b, _b) = crate::services::tests::client(test_db.clone(), manager.character).await;
        let (mut c, _c) = crate::services::tests::client(test_db.clone(), member.character).await;
        let (mut d, _d) = crate::services::tests::client(test_db.clone(), outsider.character).await;
        let group = rpc(&mut a, "Create", &[Value::unicode("group"), Value::unicode("description")]).await.unwrap().as_int().unwrap();
        assert_eq!(access_groups::membership(&test_db, owner.character, group).await.unwrap(), ADMIN);
        let metadata = rpc(&mut d, "GetGroup", &[Value::Int(group)]).await.unwrap();
        assert_eq!(crate::services::value_field(&metadata, "creatorID").and_then(Value::as_int), Some(owner.character));
        assert_eq!(crate::services::value_field(&metadata, "membershipType").and_then(Value::as_int), Some(NONE.into()));
        assert_eq!(rpc(&mut d, "GetGroupsMany", &[ids(&[group])]).await.unwrap().items().unwrap().len(), 1);
        assert!(rpc(&mut d, "GetMembers", &[Value::Int(group)]).await.is_err());
        assert!(rpc(&mut d, "GetGroupLogs", &[Value::Int(group)]).await.is_err());
        assert!(rpc(&mut d, "GetMembersForMultipleGroups", &[ids(&[group])]).await.is_err());
        assert_eq!(rpc(&mut a, "AddMembers", &[Value::Int(group), grants(&[(manager.character, MANAGER), (member.character, MEMBER)])]).await.unwrap(), Value::list([]));
        for (client, expected) in [(&mut a, ADMIN), (&mut b, MANAGER), (&mut c, MEMBER), (&mut d, NONE)] {
            for row in [rpc(client, "GetGroup", &[Value::Int(group)]).await.unwrap(),
                rpc(client, "GetGroupsMany", &[ids(&[group])]).await.unwrap().items().unwrap()[0].clone()] {
                assert_eq!(crate::services::value_field(&row, "groupID"), Some(&Value::Int(group)));
                assert_eq!(crate::services::value_field(&row, "name"), Some(&Value::unicode("group")));
                assert_eq!(crate::services::value_field(&row, "description"), Some(&Value::unicode("description")));
                assert_eq!(crate::services::value_field(&row, "creatorID"), Some(&Value::Int(owner.character)));
                assert_eq!(crate::services::value_field(&row, "membershipType"), Some(&Value::Int(expected.into())));
                assert_eq!(crate::services::value_field(&row, "admins"), Some(&ids(&[owner.character])));
            }
        }
        let multi = rpc(&mut a, "GetMembersForMultipleGroups", &[ids(&[group])]).await.unwrap();
        assert!(matches!(&multi, Value::Dict(rows) if rows.len() == 1 && rows[0].0 == Value::Int(group) && rows[0].1.items().unwrap().len() == 3));
        let Value::Dict(groups) = &multi else { panic!("expected group member mapping") };
        let members = groups[0].1.items().unwrap();
        for (id, kind) in [(owner.character, ADMIN), (manager.character, MANAGER), (member.character, MEMBER)] {
            let row = members.iter().find(|row| crate::services::value_field(row, "memberID") == Some(&Value::Int(id))).unwrap();
            assert_eq!(crate::services::value_field(row, "membershipType"), Some(&Value::Int(kind.into())));
        }
        let before = crate::dao::access_groups::logs(&test_db, group).await.unwrap().len();
        assert!(rpc(&mut c, "UpdateName", &[Value::Int(group), Value::unicode("forbidden")]).await.is_err());
        for kind in 0..=3 {
            assert!(rpc(&mut c, "UpdateMembershipTypes", &[Value::Int(group), ids(&[member.character]), Value::Int(kind)]).await.is_err());
            assert_eq!(access_groups::membership(&test_db, member.character, group).await.unwrap(), MEMBER);
        }
        assert!(rpc(&mut c, "DeleteMembers", &[Value::Int(group), ids(&[owner.character])]).await.is_err());
        assert_eq!(crate::dao::access_groups::logs(&test_db, group).await.unwrap().len(), before);
        let refusal = rpc(&mut b, "UpdateMembershipTypes", &[Value::Int(group), ids(&[owner.character]), Value::Int(MEMBER.into())]).await.unwrap();
        assert_eq!(refusal, Value::list([Value::tuple([Value::Int(owner.character), Value::str("NotAllowedToUpdateAdminOrManager")])]));
        let refusal = rpc(&mut a, "DeleteMembers", &[Value::Int(group), ids(&[owner.character])]).await.unwrap();
        assert_eq!(refusal, Value::list([Value::tuple([Value::Int(owner.character), Value::str("IsTheLastAdmin")])]));
        let refusal = rpc(&mut a, "AddMembers", &[Value::Int(group), grants(&[(PUBLIC, ADMIN)])]).await.unwrap();
        assert_eq!(refusal, Value::list([Value::tuple([Value::Int(PUBLIC), Value::str("AdminsAndManagersMustBeCharacters")])]));
        let refusal = rpc(&mut a, "AddMembers", &[Value::Int(group), grants(&[(i64::MAX, MEMBER)])]).await.unwrap();
        assert_eq!(refusal, Value::list([Value::tuple([Value::Int(i64::MAX), Value::str("IllegalMemberID")])]));
        assert_eq!(crate::dao::access_groups::logs(&test_db, group).await.unwrap().len(), before);
        rpc(&mut a, "UpdateName", &[Value::Int(group), Value::unicode("changed")]).await.unwrap();
        rpc(&mut a, "UpdateDescription", &[Value::Int(group), Value::unicode("new description")]).await.unwrap();
        rpc(&mut a, "UpdateMembershipTypes", &[Value::Int(group), ids(&[member.character]), Value::Int(EXCLUDED.into())]).await.unwrap();
        rpc(&mut a, "DeleteMembers", &[Value::Int(group), ids(&[member.character])]).await.unwrap();
        let logs = crate::dao::access_groups::logs(&test_db, group).await.unwrap();
        assert_eq!(logs.iter().map(|r| r.action_id).collect::<Vec<_>>(), vec![1, 2, 2, 5, 6, 3, 4]);
        assert_eq!((logs[3].old_text.as_deref(), logs[3].new_text.as_deref()), (Some("group"), Some("changed")));
        assert_eq!((logs[4].old_text.as_deref(), logs[4].new_text.as_deref()), (Some("description"), Some("new description")));
        assert_eq!((logs[5].owner_id, logs[5].old_membership_type, logs[5].new_membership_type), (Some(member.character), Some(MEMBER), Some(EXCLUDED)));
        assert_eq!((logs[6].owner_id, logs[6].old_membership_type, logs[6].new_membership_type), (Some(member.character), Some(EXCLUDED), None));
        let wire = rpc(&mut a, "GetGroupLogs", &[Value::Int(group)]).await.unwrap();
        let decoded = marshal::loads(&marshal::dumps(&wire)).unwrap();
        let Value::Reduce { data, .. } = decoded else { panic!("expected CRowset") };
        let args = data.items().unwrap()[1].items().unwrap();
        let descriptor = marshal::DbRowDescriptor::from_value(&args[0]).unwrap();
        assert_eq!(descriptor.columns, vec![
            ("logDate".into(), marshal::DbType::FileTime), ("actionID".into(), marshal::DbType::I4),
            ("characterID".into(), marshal::DbType::I8), ("ownerID".into(), marshal::DbType::I8),
            ("oldText".into(), marshal::DbType::WStr), ("newText".into(), marshal::DbType::WStr),
            ("oldMembershipType".into(), marshal::DbType::I4), ("newMembershipType".into(), marshal::DbType::I4),
        ]);
        let rows = args[1].items().unwrap();
        assert_eq!(rows.len(), logs.len());
        for (row, log) in rows.iter().zip(&logs) {
            assert_eq!(descriptor.get(row, "logDate"), Some(Value::Int(log.log_date)));
            assert_eq!(descriptor.get(row, "actionID"), Some(Value::Int(log.action_id.into())));
            assert_eq!(descriptor.get(row, "characterID"), Some(Value::Int(log.character_id)));
            assert_eq!(descriptor.get(row, "ownerID"), Some(log.owner_id.map(Value::Int).unwrap_or(Value::None)));
            assert_eq!(descriptor.get(row, "oldText"), Some(log.old_text.as_ref().map(Value::unicode).unwrap_or(Value::None)));
            assert_eq!(descriptor.get(row, "newText"), Some(log.new_text.as_ref().map(Value::unicode).unwrap_or(Value::None)));
            assert_eq!(descriptor.get(row, "oldMembershipType"), Some(log.old_membership_type.map(|v| Value::Int(v.into())).unwrap_or(Value::None)));
            assert_eq!(descriptor.get(row, "newMembershipType"), Some(log.new_membership_type.map(|v| Value::Int(v.into())).unwrap_or(Value::None)));
        }
        assert!(logs.iter().all(|r| r.log_date > 0 && r.character_id == owner.character));
        rpc(&mut a, "AddMembers", &[Value::Int(group), grants(&[(PUBLIC, MEMBER), (outsider.character, EXCLUDED)])]).await.unwrap();
        assert_eq!(access_groups::membership(&test_db, member.character, group).await.unwrap(), MEMBER);
        assert_eq!(access_groups::membership(&test_db, outsider.character, group).await.unwrap(), EXCLUDED);
        assert!(rpc(&mut d, "GetMembers", &[Value::Int(group)]).await.is_err());
        assert_eq!(rpc(&mut b, "DeleteMembers", &[Value::Int(group), ids(&[manager.character])]).await.unwrap(), Value::list([]));
        assert_eq!(access_groups::membership(&test_db, manager.character, group).await.unwrap(), MEMBER);
        rpc(&mut a, "Delete", &[Value::Int(group)]).await.unwrap();
        assert!(access_groups::membership(&test_db, member.character, group).await.is_err());
        assert_eq!(crate::dao::access_groups::logs(&test_db, group).await.unwrap().last().unwrap().action_id, 7);
        test_db.execute("DELETE FROM game.access_group_logs WHERE group_id = $1", &[&group]).await.unwrap();
        let corp_group = rpc(&mut a, "Create", &[Value::unicode("corporation"), Value::unicode("")]).await.unwrap().as_int().unwrap();
        let corporation = crate::dao::corporations::registry::membership(&test_db, member.character).await.unwrap().corporation_id;
        crate::dao::access_groups::set_member(&test_db, corp_group, corporation, EXCLUDED).await.unwrap();
        crate::dao::access_groups::set_member(&test_db, corp_group, member.character, MEMBER).await.unwrap();
        assert_eq!(access_groups::membership(&test_db, member.character, corp_group).await.unwrap(), EXCLUDED);
        test_db.execute("UPDATE game.characters SET corporation_id = $2 WHERE character_id = $1", &[&member.character, &(corporation + 1)]).await.unwrap();
        assert_eq!(access_groups::membership(&test_db, member.character, corp_group).await.unwrap(), MEMBER);
    }).await;
    cleanup(&db, &[owner.character, manager.character, member.character, outsider.character]).await;
    for f in [owner, manager, member, outsider] { fixture::cleanup(&db, f).await.unwrap(); }
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn group_role_matrix_and_live_affiliations() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let manager = fixture::create(&db).await.unwrap();
    let peer = fixture::create(&db).await.unwrap();
    let member = fixture::create(&db).await.unwrap();
    let corp: i64 = db.query_one("INSERT INTO game.corporations (name,ticker,ceo_id,creator_id,station_id,created) VALUES ('group-test-' || nextval('game.corporations_corporation_id_seq')::text, 'GT' || currval('game.corporations_corporation_id_seq')::text, $1,$1,1,1) RETURNING corporation_id", &[&owner.character]).await.unwrap().get("corporation_id");
    let alliance: i64 = db.query_one("INSERT INTO game.alliances (name,short_name,executor_corp_id,creator_corp_id,creator_char_id,created) VALUES ('group-test-' || nextval('game.alliances_alliance_id_seq')::text, 'GT' || currval('game.alliances_alliance_id_seq')::text, $1,$1,$2,1) RETURNING alliance_id", &[&corp, &owner.character]).await.unwrap().get("alliance_id");
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        test_db.execute("UPDATE game.characters SET corporation_id = $2 WHERE character_id = $1", &[&member.character, &corp]).await.unwrap();
        test_db.execute("UPDATE game.corporations SET alliance_id = $2 WHERE corporation_id = $1", &[&corp, &alliance]).await.unwrap();
        let (mut a, _a) = crate::services::tests::client(test_db.clone(), owner.character).await;
        let (mut b, _b) = crate::services::tests::client(test_db.clone(), manager.character).await;
        let (mut c, _c) = crate::services::tests::client(test_db.clone(), member.character).await;
        let group = rpc(&mut a, "Create", &[Value::unicode("matrix"), Value::unicode("")]).await.unwrap().as_int().unwrap();
        assert_eq!(rpc(&mut a, "AddMembers", &[Value::Int(group), grants(&[(manager.character, MANAGER), (peer.character, MANAGER), (member.character, MEMBER)])]).await.unwrap(), Value::list([]));
        let before = crate::dao::access_groups::logs(&test_db, group).await.unwrap().len();
        for kind in [MANAGER, ADMIN] {
            let actual = rpc(&mut b, "UpdateMembershipTypes", &[Value::Int(group), ids(&[member.character]), Value::Int(kind.into())]).await.unwrap();
            assert_eq!(actual, Value::list([Value::tuple([Value::Int(member.character), Value::str("NotAllowedToPromoteToAdmin")])]));
            let actual = rpc(&mut b, "AddMembers", &[Value::Int(group), grants(&[(owner.character, kind)])]).await.unwrap();
            assert_eq!(actual, Value::list([Value::tuple([Value::Int(owner.character), Value::str("NotAllowedToUpdateAdminOrManager")])]));
            let actual = rpc(&mut b, "AddMembers", &[Value::Int(group), grants(&[(member.character, kind)])]).await.unwrap();
            assert_eq!(actual, Value::list([Value::tuple([Value::Int(member.character), Value::str("ManagersCanNotAddAdminsOrManagers")])]));
        }
        for target in [peer.character, owner.character] {
            for method in ["UpdateMembershipTypes", "DeleteMembers"] {
                let mut args = vec![Value::Int(group), ids(&[target])];
                if method == "UpdateMembershipTypes" { args.push(Value::Int(MEMBER.into())); }
                assert_eq!(rpc(&mut b, method, &args).await.unwrap(), Value::list([Value::tuple([Value::Int(target), Value::str("NotAllowedToUpdateAdminOrManager")])]));
            }
        }
        for target in [PUBLIC, corp, alliance] {
            for kind in [MANAGER, ADMIN] {
                assert_eq!(rpc(&mut a, "AddMembers", &[Value::Int(group), grants(&[(target, kind)])]).await.unwrap(), Value::list([Value::tuple([Value::Int(target), Value::str("AdminsAndManagersMustBeCharacters")])]));
            }
        }
        assert_eq!(crate::dao::access_groups::logs(&test_db, group).await.unwrap().len(), before);
        assert_eq!(access_groups::membership(&test_db, peer.character, group).await.unwrap(), MANAGER);
        assert_eq!(access_groups::membership(&test_db, member.character, group).await.unwrap(), MEMBER);
        assert_eq!(rpc(&mut c, "DeleteMembers", &[Value::Int(group), ids(&[member.character])]).await.unwrap(), Value::list([]));
        assert_eq!(rpc(&mut b, "DeleteMembers", &[Value::Int(group), ids(&[manager.character])]).await.unwrap(), Value::list([]));
        for method in ["UpdateMembershipTypes", "DeleteMembers"] {
            let mut args = vec![Value::Int(group), ids(&[owner.character])];
            if method == "UpdateMembershipTypes" { args.push(Value::Int(MEMBER.into())); }
            assert_eq!(rpc(&mut a, method, &args).await.unwrap(), Value::list([Value::tuple([Value::Int(owner.character), Value::str("IsTheLastAdmin")])]));
        }
        assert_eq!(rpc(&mut a, "UpdateMembershipTypes", &[Value::Int(group), ids(&[peer.character]), Value::Int(ADMIN.into())]).await.unwrap(), Value::list([]));
        assert_eq!(rpc(&mut a, "DeleteMembers", &[Value::Int(group), ids(&[owner.character, peer.character])]).await.unwrap(), Value::list([Value::tuple([Value::Int(peer.character.max(owner.character)), Value::str("IsTheLastAdmin")])]));

        let live = rpc(&mut a, "Create", &[Value::unicode("live"), Value::unicode("")]).await.unwrap().as_int().unwrap();
        assert_eq!(access_groups::membership(&test_db, member.character, live).await.unwrap(), NONE);
        for entity in [PUBLIC, alliance, corp] {
            assert_eq!(rpc(&mut a, "AddMembers", &[Value::Int(live), grants(&[(entity, MEMBER)])]).await.unwrap(), Value::list([]));
            assert_eq!(access_groups::membership(&test_db, member.character, live).await.unwrap(), MEMBER);
            assert_eq!(rpc(&mut a, "DeleteMembers", &[Value::Int(live), ids(&[entity])]).await.unwrap(), Value::list([]));
        }
        assert_eq!(rpc(&mut a, "AddMembers", &[Value::Int(live), grants(&[(PUBLIC, MEMBER), (alliance, MEMBER), (corp, MEMBER), (member.character, MANAGER)])]).await.unwrap(), Value::list([]));
        assert_eq!(access_groups::membership(&test_db, member.character, live).await.unwrap(), MANAGER);
        for entity in [alliance, corp, member.character] {
            assert_eq!(rpc(&mut a, "UpdateMembershipTypes", &[Value::Int(live), ids(&[entity]), Value::Int(EXCLUDED.into())]).await.unwrap(), Value::list([]));
            assert_eq!(access_groups::membership(&test_db, member.character, live).await.unwrap(), EXCLUDED);
            let restore = if entity == member.character { MANAGER } else { MEMBER };
            assert_eq!(rpc(&mut a, "UpdateMembershipTypes", &[Value::Int(live), ids(&[entity]), Value::Int(restore.into())]).await.unwrap(), Value::list([]));
        }
        assert_eq!(rpc(&mut a, "DeleteMembers", &[Value::Int(live), ids(&[PUBLIC, corp, member.character])]).await.unwrap(), Value::list([]));
        assert_eq!(access_groups::membership(&test_db, member.character, live).await.unwrap(), MEMBER);
        test_db.execute("UPDATE game.corporations SET alliance_id = NULL WHERE corporation_id = $1", &[&corp]).await.unwrap();
        assert_eq!(access_groups::membership(&test_db, member.character, live).await.unwrap(), NONE);
        assert_eq!(rpc(&mut a, "AddMembers", &[Value::Int(live), grants(&[(PUBLIC, MEMBER)])]).await.unwrap(), Value::list([]));
        assert_eq!(rpc(&mut a, "UpdateMembershipTypes", &[Value::Int(live), ids(&[PUBLIC]), Value::Int(EXCLUDED.into())]).await.unwrap(), Value::list([]));
        assert_eq!(access_groups::membership(&test_db, member.character, live).await.unwrap(), EXCLUDED);
        assert_eq!(access_groups::membership(&test_db, owner.character, live).await.unwrap(), EXCLUDED);
        let before = crate::dao::access_groups::logs(&test_db, live).await.unwrap().len();
        assert!(rpc(&mut a, "UpdateMembershipTypes", &[Value::Int(live), ids(&[PUBLIC]), Value::Int(MEMBER.into())]).await.is_err());
        assert_eq!(crate::dao::access_groups::logs(&test_db, live).await.unwrap().len(), before);
        assert!(crate::dao::access_groups::members(&test_db, live).await.unwrap().iter().any(|row| row.member_id == PUBLIC && row.membership_type == EXCLUDED));
    }).await;
    cleanup(&db, &[owner.character, manager.character, peer.character, member.character]).await;
    db.execute("UPDATE game.characters SET corporation_id = 1 WHERE character_id = $1", &[&member.character]).await.unwrap();
    db.execute("UPDATE game.corporations SET alliance_id = NULL WHERE corporation_id = $1", &[&corp]).await.unwrap();
    db.execute("DELETE FROM game.alliances WHERE alliance_id = $1", &[&alliance]).await.unwrap();
    db.execute("DELETE FROM game.corporations WHERE corporation_id = $1", &[&corp]).await.unwrap();
    for f in [owner, manager, peer, member] { fixture::cleanup(&db, f).await.unwrap(); }
    result.unwrap();
}
