//! Completion event ordering and duplicate suppression for lazy skill training.
use super::*;

mod atomicity;

#[test]
fn completed_levels_are_sorted_and_only_include_new_levels() {
    let done = [(2, 2, 20), (1, 2, 20), (1, 1, 10), (2, 2, 20), (2, 1, 10), (1, 3, 30)];
    let levels = HashMap::from([(1, 1), (2, 0)]);
    assert_eq!(completed_levels(&done, levels), vec![(1, 2), (1, 3), (2, 1), (2, 2)]);
}
