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
