use super::*;

#[test]
fn period_start_is_a_utc_timestamp() {
    let p = period_start();
    assert_eq!(p.len(), 20);
    assert!(p.ends_with("T03:00:00Z"), "{p}");
}

#[test]
fn rewards_parse() {
    assert_eq!(crate::sde::store::parse_rewards(Some("57003:1,72409:2,bad,12:x".into())), vec![(57003, 1), (72409, 2)]);
    assert!(crate::sde::store::parse_rewards(None).is_empty());
}
