use super::*;

fn fleet() -> Fleet {
    let mut f = Fleet::new(1, vec![Wing { id: 10, name: String::new(), squads: vec![Squad { id: 11, name: String::new() }, Squad { id: 12, name: String::new() }] }]);
    f.members.insert(1, Member::new(NONE_ID, NONE_ID, ROLE_LEADER, JOB_CREATOR, 0));
    f.members.insert(2, Member::new(10, 11, ROLE_MEMBER, JOB_NONE, 3));
    f.members.insert(3, Member::new(10, 12, ROLE_SQUAD_COMMANDER, JOB_NONE, 4));
    f
}

#[test]
fn moves_validate_topology_and_roll_back_the_entire_batch() {
    let mut f = fleet();
    let mut moved = f.members[&2].clone();
    moved.squad = 12;
    let mut invalid = f.members[&3].clone();
    invalid.squad = 99;
    assert!(apply(&mut f, vec![(2, moved.clone()), (3, invalid)]).is_none());
    assert_eq!(f.members[&2].squad, 11);
    assert_eq!(f.members[&3].squad, 12);
    let changes = apply(&mut f, vec![(2, moved)]).unwrap();
    assert_eq!((changes[0].1.squad, changes[0].2.squad), (11, 12));
    assert_eq!(changed(2, 9, &changes[0].1, &changes[0].2, false)[8], Value::Int(12));
}

#[test]
fn duplicate_commanders_and_leader_loss_are_rejected() {
    let mut f = fleet();
    let mut promoted = f.members[&2].clone();
    promoted.squad = 12;
    promoted.role = ROLE_SQUAD_COMMANDER;
    assert!(apply(&mut f, vec![(2, promoted)]).is_none());
    let mut demoted = f.members[&1].clone();
    demoted.wing = 10; demoted.squad = 11; demoted.role = ROLE_MEMBER;
    assert!(apply(&mut f, vec![(1, demoted)]).is_none());
    assert_eq!(f.members[&1].role, ROLE_LEADER);
}

#[test]
fn placement_respects_requested_mode_and_rejects_insufficient_squads() {
    let mut f = fleet();
    f.members.insert(4, Member::new(10, 11, ROLE_MEMBER, JOB_NONE, 5));
    let ids = [2, 4];
    let even = plan(&f, 10, &ids, "ArrangeMembersByEvenlyDistribute", 1).unwrap();
    assert_eq!((even[0].1.squad, even[1].1.squad), (11, 12));
    let first = plan(&f, 10, &ids, "ArrangeMembersByAllToFirst", 1).unwrap();
    assert_eq!((first[0].1.squad, first[1].1.squad), (11, 11));
    assert!(plan(&f, 10, &[2, 4, 3], "ArrangeMembersByPreCount", 1).is_none());
    assert_eq!(f.members[&2].squad, 11);
    assert_eq!(f.members[&4].squad, 11);
}
