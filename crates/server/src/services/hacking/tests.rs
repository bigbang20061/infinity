use super::*;

#[test]
fn boards_connect_entry_to_core() {
    for tier in 1..=4 {
        let mut id = 0;
        let (tiles, start) = board(tier, || {
            id += 1;
            id
        });
        let core = tiles.values().filter(|t| t.kind == TYPE_CORE).count();
        assert_eq!(core, 1);
        assert!(!tiles[&start].hidden);
        let g = Game {
            container: 0,
            system: 0,
            tier,
            tiles,
            coherence: 100,
            max_coherence: 100,
            strength: 30,
            inventory: vec![],
            next_id: id,
            skip_click: None,
        };
        assert!(g.distance_to_core(start).is_some_and(|d| d > 0));
        assert!(neighbours(start).iter().any(|n| g.flippable(*n)));
    }
}

#[test]
fn analyzer_skill_matches_postgres_integer_cast() {
    assert_eq!(analyzer_skill(2.5).ok(), Some(2));
    assert_eq!(analyzer_skill(3.5).ok(), Some(4));
    assert_eq!(analyzer_skill(-2.5).ok(), Some(-2));
    assert_eq!(analyzer_skill(i32::MIN as f64).ok(), Some(i32::MIN));
    assert_eq!(analyzer_skill(i32::MAX as f64).ok(), Some(i32::MAX));
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, i32::MIN as f64 - 1.0, i32::MAX as f64 + 1.0] {
        assert!(matches!(analyzer_skill(value), Err(CallError::Internal(_))));
    }
}
