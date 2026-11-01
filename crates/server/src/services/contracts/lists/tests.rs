use super::{location_matches, type_matches};

mod corporate;

#[test]
fn location_filter_handles_missing_ancestry_and_direct_station() {
    assert!(location_matches(Some(9), 9, [None, None, None]));
    assert!(location_matches(Some(3), 9, [Some(2), Some(3), Some(4)]));
    assert!(!location_matches(Some(0), 9, [None, None, None]));
    assert!(location_matches(None, 9, [None, None, None]));
}

#[test]
fn type_filter_respects_precedence_and_nulls() {
    assert!(type_matches(7, Some(2), Some(3), Some(&[7]), Some(8), Some(9)));
    assert!(!type_matches(7, Some(2), Some(3), Some(&[]), Some(2), Some(3)));
    assert!(!type_matches(7, None, Some(3), None, Some(2), Some(3)));
    assert!(type_matches(7, None, Some(3), None, None, Some(3)));
}


#[test]
fn location_filter_precedes_limit_and_keeps_candidate_order() {
    let candidates = [(10, 1), (11, 2), (12, 2), (13, 2)];
    let page: Vec<_> = candidates.into_iter()
        .filter(|(_, station)| location_matches(Some(2), *station, [None; 3]))
        .take(2)
        .map(|(id, _)| id)
        .collect();
    assert_eq!(page, [11, 12]);
}