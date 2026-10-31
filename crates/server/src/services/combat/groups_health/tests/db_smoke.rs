use super::*;
use std::sync::Arc;
use crate::space::{Ball, WarpState, MODE_STOP};

fn ball(id: i64, type_id: i32, pilot: Option<i64>) -> Ball {
    Ball {
        id, type_id, owner_id: pilot.unwrap_or(1), corp_id: 1, alliance_id: -1,
        char_id: pilot, signature_radius: None, mode: MODE_STOP, radius: 1.0,
        mass: 1e6, max_vel: 0.0, agility: 1.0, speed_fraction: 0.0,
        pos: [0.0; 3], vel: [0.0; 3], old_pos: [0.0; 3], old_vel: [0.0; 3],
        segment_acceleration: None, goto: [0.0; 3], follow_id: 0, follow_range: 0.0,
        free: false, global: false, massive: false, interactive: false,
        warp: WarpState::default(), warp_factor: 0, last_ms: 0, interpolation_anchor_ms: 0,
        effect_stamp: 0, cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new(),
    }
}

#[test]
fn npc_take_replace_and_destroy_forget_combat_state_but_player_take_preserves_it() {
    use crate::space::{NpcBrain, NpcConfig, System};
    crate::tests::init_config();
    let mut system = System::default();
    let ids = [i64::MIN + 100, i64::MIN + 101, i64::MIN + 102, i64::MIN + 103];
    for &id in &ids {
        system.insert(ball(id, 1, None));
        assert!(system.add_npc(id, NpcBrain::new(None, NpcConfig {
            fly_range: 1.0, aggro_range: 1.0, rate_of_fire_ms: 1.0,
            scan_resolution: 1.0, repairs: (0.0, 0.0, 1.0),
        }, Arc::new(|_| Box::pin(async {})))));
    }
    let player = i64::MIN + 104;
    system.insert(ball(player, 1, Some(1)));
    {
        let mut st = state();
        for id in ids.into_iter().chain([player]) {
            st.capacitor.insert(id, (0.5, 0));
            st.hp.insert(id, Hp { now: [0.5; 3], max: [1.0; 3], resonance: [[1.0; 4]; 3] });
            st.condition.insert(id, Condition {
                stats: ShipStats { max: [1.0; 3], resonance: [[1.0; 4]; 3], shield_tau_ms: 1.0 },
                now: [0.5; 3], shield_at: 0,
            });
        }
    }
    system.take(ids[0]).unwrap();
    system.insert(ball(ids[1], 1, None));
    system.destroy_ball(ids[2]).unwrap();
    system.remove_ball(ids[3]).unwrap();
    system.take(player).unwrap();
    let mut st = state();
    for id in ids {
        assert!(!st.capacitor.contains_key(&id));
        assert!(!st.condition.contains_key(&id));
        assert!(!st.hp.contains_key(&id));
    }
    assert!(st.capacitor.remove(&player).is_some());
    assert!(st.condition.remove(&player).is_some());
    assert!(st.hp.remove(&player).is_some());
}
