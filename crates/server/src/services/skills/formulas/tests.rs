use super::{level_from_points, sp_for_level};

#[test]
fn skill_levels_follow_point_thresholds() {
    for rank in [1.0, 2.0, 3.0, 5.0] {
        for level in 1..=5 {
            let threshold = sp_for_level(rank, level);
            assert_eq!(level_from_points(threshold - 1, rank), (level - 1) as i32, "rank {rank}, level {level}, below");
            assert_eq!(level_from_points(threshold, rank), level as i32, "rank {rank}, level {level}, at");
        }
    }
}

#[test]
fn zero_rank_and_maximum_level() {
    assert_eq!(level_from_points(0, 0.0), 0);
    assert_eq!(level_from_points(1_000_000, 0.0), 0);
    assert_eq!(level_from_points(i64::MAX, 1.0), 5);
}
