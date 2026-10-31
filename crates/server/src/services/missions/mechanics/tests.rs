//! Eligibility preserves JSON key presence separately from its nullable value.
use super::{eligible_kill, rewards};
use crate::sde::{dungeons::Dungeon, missions::{Mission, JournalMessage}};

#[test]
fn kill_eligibility_uses_key_presence_and_difficulty() {
    let mut m = Mission { mission_id: 1, name_id: None, has_name: true, template: Some("agent.missionTemplatizedContent_BasicKillMission".into()), dungeon_id: Some(1), faction_id: None, cargo_type: None, cargo_qty: None, reward_type: None, reward_quantity: None, reward_lp: None, messages: Vec::new() };
    let d = Dungeon { id: 1, name_id: None, has_name: false, difficulty: Some(2), faction_id: None, archetype: None, gated: false };
    assert!(!eligible_kill(&m, &d, 2));
    m.messages.push(JournalMessage { key: "messages.mission.briefing".into(), mission_id: 1, message_id: None });
    assert!(eligible_kill(&m, &d, 2));
    assert!(!eligible_kill(&m, &d, 1));
    m.has_name = false;
    assert!(!eligible_kill(&m, &d, 2));
}

#[test]
fn rewards_use_level_table_and_clamp_high_levels() {
    crate::tests::init_config();
    let table = &crate::config::game().missions.rewards;
    for level in 1..=5 {
        let reward = &table[(level - 1) as usize];
        assert_eq!(rewards(level), (reward.isk, reward.lp));
    }
    assert_eq!(rewards(i64::MAX), rewards(5));
    assert_eq!(rewards(0), rewards(5));
}
