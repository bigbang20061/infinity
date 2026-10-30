use super::*;

#[test]
fn weekly_reset_is_next_monday() {
    // 2026-10-27 08:00 UTC is a Tuesday; the next reset is Monday 2026-11-02 03:00 UTC.
    assert_eq!(next_weekly_reset(1_793_088_000), 1_793_588_400);
}
