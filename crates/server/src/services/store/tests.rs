use super::*;

#[test]
fn period_start_is_a_utc_timestamp() {
    crate::tests::init_config();
    let p = period_start();
    assert_eq!(p.len(), 20);
    assert!(p.ends_with("T03:00:00Z"), "{p}");
}

#[test]
fn period_start_respects_turnover_boundary() {
    assert_eq!(period_start_at(10_799, "03:00:00"), "1969-12-31T03:00:00Z");
    assert_eq!(period_start_at(10_800, "03:00:00"), "1970-01-01T03:00:00Z");
    assert_eq!(period_start_at(45_296, "12:34:56"), "1970-01-01T12:34:56Z");
}

#[test]
fn rewards_parse() {
    assert_eq!(crate::sde::store::parse_rewards(Some("57003:1,72409:2,bad,12:x".into())), vec![(57003, 1), (72409, 2)]);
    assert!(crate::sde::store::parse_rewards(None).is_empty());
}
