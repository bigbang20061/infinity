use super::*;
use crate::sde::dungeons::Dungeon;

fn dungeon(id: i64, archetype: &str, name_id: Option<i64>, has_name: bool) -> Dungeon {
    Dungeon { id, name_id, has_name, difficulty: None, faction_id: Some(7), archetype: Some(archetype.into()), gated: false }
}

#[test]
fn combat_preserves_null_name_and_sleeper_prefix_rules() {
    let d = dungeon(1, "24", None, true);
    assert!(combat_candidate(&d, None, 7, (1, 1), false, false));
    assert!(!combat_candidate(&d, None, 7, (1, 1), false, true));
    assert!(combat_candidate(&d, Some("Unsecured Frontier Stronghold"), 7, (1, 1), false, true));
    assert!(!combat_candidate(&d, Some("Other Frontier"), 7, (1, 1), false, true));
    assert!(!combat_candidate(&d, Some("Frontier"), 7, (2, 3), false, true));
}

#[test]
fn mining_name_matches_sql_patterns() {
    assert!(mining_name("Ancient Deposit", "Deposit", false));
    assert!(!mining_name("OLD Deposit", "Deposit", false));
    assert!(mining_name("Azure Nebula", " Nebula", true));
    assert!(!mining_name("Azure Nebula II", " Nebula", true));
}

#[test]
fn hacking_prefers_archetype_before_random_and_only_then_falls_back() {
    let rows = [dungeon(1, "24", Some(1), true), dungeon(2, "45", Some(2), true), dungeon(3, "45", None, true), dungeon(4, "45", None, false)];
    let names = |id| match id { 1 => Some("Local Site"), 2 => Some("Local Data"), _ => None };
    let ids = |v: Vec<&Dungeon>| v.into_iter().map(|d| d.id).collect::<Vec<_>>();
    assert_eq!(ids(hack_candidates(rows.iter(), names, 7, "45", "Local")), vec![2]);
    assert_eq!(ids(hack_candidates(rows.iter(), names, 7, "44", "Local")), vec![1, 2]);
    assert_eq!(ids(hack_candidates(rows.iter(), names, 7, "45", "Missing")), vec![2, 3]);
    assert!(!hack_name("Local Site: Annex", "Local"));
    assert!(!hack_name("Locally Site", "Local"));
}
