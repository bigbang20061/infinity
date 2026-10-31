use super::*;

mod atomicity;

#[test]
fn retail_agents_load() {
    let a = agent(3009965).expect("Luminaire security agent");
    assert_eq!((a.type_id, a.level, a.station_id), (2, 1, 60012007));
}

#[test]
fn epic_arc_agents_are_known() {
    // Sister Alitura (Blood-Stained Stars) and the Caldari arc's first agent.
    for id in [3019356, 3019372] {
        assert!(agent(id).is_some(), "{id}");
    }
    assert_eq!(Kind::of_template("agent.missionTemplatizedContent_EpicArcTalkToAgentMission"), Kind::Talk);
    assert_eq!(Kind::of_template("agent.missionTemplatizedContent_BasicKillMission"), Kind::Kill);
}

#[test]
fn npc_classes_match_group_names() {
    assert!(group_has_class("Mission Caldari State Cruiser", "Cruiser"));
    assert!(!group_has_class("Mission Caldari State Battlecruiser", "Cruiser"));
    assert!(group_has_class("Mission Generic Battle Cruisers", "Battlecruiser"));
    assert!(group_has_class("Deadspace Guristas Frigate", "Frigate"));
    assert_eq!(spawn_classes(1).len(), 4);
}
