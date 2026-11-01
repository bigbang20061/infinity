use super::*;
use super::super::broadcasts::allowed;

#[test]
fn hierarchy_matches_client_superior_and_subordinate_rules() {
    let leader = Member::new(NONE_ID, NONE_ID, ROLE_LEADER, JOB_NONE, 0);
    let wing = Member::new(10, NONE_ID, ROLE_WING_COMMANDER, JOB_NONE, 0);
    let squad = Member::new(10, 11, ROLE_SQUAD_COMMANDER, JOB_NONE, 0);
    let peer = Member::new(10, 11, ROLE_MEMBER, JOB_NONE, 0);
    let other_squad = Member::new(10, 12, ROLE_MEMBER, JOB_NONE, 0);
    let other_wing = Member::new(20, 21, ROLE_SQUAD_COMMANDER, JOB_NONE, 0);
    assert!(allowed(2, 4, &peer, 1, &leader));
    assert!(allowed(2, 4, &peer, 2, &wing));
    assert!(allowed(2, 4, &peer, 3, &squad));
    assert!(!allowed(2, 4, &peer, 6, &other_wing));
    assert!(!allowed(2, 2, &wing, 3, &squad));
    assert!(allowed(1, 2, &wing, 5, &other_squad));
    assert!(!allowed(1, 3, &squad, 5, &other_squad));
    assert!(allowed(1, 3, &squad, 4, &peer));
    assert!(allowed(1, 1, &leader, 6, &other_wing));
    assert!(!allowed(2, 1, &leader, 2, &wing));
    assert!(allowed(3, 4, &peer, 6, &other_wing));
    assert!(allowed(2, 4, &peer, 4, &peer));
}
