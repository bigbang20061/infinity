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
    let rows = [
        ("Mission Generic Cruiser", true),
        ("Mission Caldari Cruiser", false),
        ("Mission Generic Frigate", true),
        ("Mission Caldari Battle Cruiser", false),
    ];
    fn candidate<'a>(r: &'a (&'static str, bool)) -> (&'a str, bool) { (r.0, r.1) }
    assert_eq!(preferred(&rows, "Cruiser", candidate).iter().map(|r| r.0).collect::<Vec<_>>(), vec!["Mission Caldari Cruiser"]);
    assert_eq!(preferred(&rows, "Frigate", candidate).iter().map(|r| r.0).collect::<Vec<_>>(), vec!["Mission Generic Frigate"]);
    assert!(preferred(&rows, "Destroyer", candidate).is_empty());
}
