use super::*;

#[test]
fn period_start_is_a_utc_timestamp() {
    let p = period_start();
    assert_eq!(p.len(), 20);
    assert!(p.ends_with("T03:00:00Z"), "{p}");
}

#[test]
fn rewards_parse() {
    assert_eq!(parse_rewards("57003:1,72409:2"), vec![(57003, 1), (72409, 2)]);
    assert!(parse_rewards("").is_empty());
}
