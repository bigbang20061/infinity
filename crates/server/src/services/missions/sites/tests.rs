use super::*;

#[test]
fn candidate_groups_follow_sql_patterns() {
    assert_eq!(candidate_group("mission caldari state cruiser", "caldari state"), Some(false));
    assert_eq!(candidate_group("deadspace caldari battleship", "caldari state"), Some(false));
    assert_eq!(candidate_group("mission generic cruisers", "caldari state"), Some(true));
    assert_eq!(candidate_group("mission caldari statehood cruiser", "caldari state"), None);
    assert_eq!(candidate_group("mission guristas cruiser", "caldari state"), None);
    assert_eq!(candidate_group("mission xabc cruiser", "x%"), Some(false));
    assert_eq!(candidate_group("deadspace ~ frigate", "~"), Some(false));
    assert_eq!(candidate_group("mission istanbul cruiser", "istanbul"), Some(false));
}

#[test]
fn faction_candidates_are_preferred_per_class() {
    let npc = |type_id, name, generic| Npc {
        type_id, name, generic, radius: 50.0, mass: 1_000_000.0,
        velocity: None, agility: None,
    };
    let rows = [
        npc(1, "Mission Generic Cruiser", true),
        npc(2, "Mission Caldari Cruiser", false),
        npc(3, "Mission Generic Frigate", true),
        npc(4, "Mission Caldari Battle Cruiser", false),
    ];
    assert_eq!(preferred(&rows, "Cruiser").iter().map(|r| r.type_id).collect::<Vec<_>>(), vec![2]);
    assert_eq!(preferred(&rows, "Frigate").iter().map(|r| r.type_id).collect::<Vec<_>>(), vec![3]);
    assert!(preferred(&rows, "Destroyer").is_empty());
}
