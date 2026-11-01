use super::*;
use crate::services::drones::settings::{self, DroneSettings, Patch};
use crate::space::npc::{NpcBrain, NpcConfig};

fn ball(id: i64, pilot: Option<i64>, x: f64) -> Ball {
    Ball {
        id, type_id: 1, owner_id: pilot.unwrap_or(1), corp_id: 1, alliance_id: -1,
        char_id: pilot, signature_radius: Some(1.0), mode: MODE_STOP, radius: 1.0, mass: 1e6,
        max_vel: 0.0, agility: 1.0, speed_fraction: 0.0,
        pos: [x, 0.0, 0.0], vel: [0.0; 3], goto: [0.0; 3],
        old_pos: [x, 0.0, 0.0], old_vel: [0.0; 3], segment_acceleration: None,
        follow_id: 0, follow_range: 0.0, free: true, global: false, interactive: pilot.is_some(),
        massive: true, warp: crate::space::WarpState::default(), warp_factor: 0, last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0,
        cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new(),
    }
}

fn spec() -> DroneSpec {
    DroneSpec { type_id: 1, bandwidth: 5.0, scan_resolution: 100.0, control_range: 1000.0,
        max_target_range: 1000.0, optimal_range: 100.0, orbit_range: 50.0, duration_ms: 1000.0, effect_id: Some(1) }
}

fn setup(base: i64) -> System {
    crate::tests::init_config();
    let mut s = System::new(1, Arc::default(), 1000);
    s.insert(ball(base, Some(base + 1), 0.0));
    let token = s.reserve_drone_launch(base + 1, base, &[5.0, 5.0], 2.0, 10.0).unwrap();
    let mut first = ball(base + 10, None, 10.0); first.owner_id = base + 1;
    let mut second = ball(base + 11, None, 10.0); second.owner_id = base + 1;
    let effects: DroneEffects = Arc::new(|_| Box::pin(async {}));
    let _ack = s.complete_drone_launch(token, base + 1, base, vec![(second, spec()), (first, spec())], effects);
    s
}

fn npc(s: &mut System, id: i64, x: f64) {
    s.insert(ball(id, None, x));
    assert!(s.add_npc(id, NpcBrain::new(None, NpcConfig { fly_range: 20.0, aggro_range: 100.0,
        rate_of_fire_ms: 2500.0, scan_resolution: 100.0, repairs: (0.0, 0.0, 5000.0) },
        Arc::new(|_| Box::pin(async {})))));
}

fn preferences(owner: i64, aggressive: bool, focus_fire: bool) {
    settings::apply(owner, DroneSettings::default(), Patch { aggressive: Some(aggressive), focus_fire: Some(focus_fire) });
}

#[test]
fn passive_records_npc_attack_and_aggressive_idle_drones_engage() {
    let base = -720000; let owner = base + 1;
    settings::logged_off(owner);
    let mut s = setup(base); npc(&mut s, base + 40, 20.0);
    s.drone_attack(base + 40, base);
    s.think_drones();
    assert_eq!(s.drone(base + 10).unwrap().order, DroneOrder::Idle);
    preferences(owner, true, true);
    s.think_drones();
    assert_eq!(s.drone(base + 10).unwrap().order, DroneOrder::Engage(base + 40));
    settings::logged_off(owner);
}

#[test]
fn focus_and_round_robin_use_sorted_physical_ids() {
    for (base, focus) in [(-721000, true), (-722000, false)] {
        let owner = base + 1; settings::logged_off(owner);
        let mut s = setup(base); npc(&mut s, base + 41, 20.0); npc(&mut s, base + 40, 20.0);
        s.drone_attack(base + 41, base); s.drone_attack(base + 40, base);
        preferences(owner, true, focus); s.think_drones();
        assert_eq!(s.drone(base + 10).unwrap().order, DroneOrder::Engage(base + 40));
        assert_eq!(s.drone(base + 11).unwrap().order, DroneOrder::Engage(base + if focus { 40 } else { 41 }));
        settings::logged_off(owner);
    }
}

#[test]
fn explicit_orders_and_ineligible_sources_are_not_retargeted() {
    let base = -723000; let owner = base + 1;
    settings::logged_off(owner);
    let mut s = setup(base); npc(&mut s, base + 40, 2000.0); npc(&mut s, base + 41, 20.0);
    s.insert(ball(base + 42, Some(base + 2), 20.0));
    s.drone_attack(base + 40, base); s.drone_attack(base + 41, base); s.drone_attack(base + 42, base);
    s.remove_ball(base + 41);
    assert!(s.drone_order(base + 10, owner, base, DroneOrder::Guard(owner)));
    preferences(owner, true, true); s.think_drones();
    assert_eq!(s.drone(base + 10).unwrap().order, DroneOrder::Guard(owner));
    assert_eq!(s.drone(base + 11).unwrap().order, DroneOrder::Idle);
    settings::logged_off(owner);
}

#[test]
fn jam_and_controller_loss_prevent_acquisition() {
    let base = -724000; let owner = base + 1;
    settings::logged_off(owner);
    let mut s = setup(base); npc(&mut s, base + 40, 20.0);
    s.drone_attack(base + 40, base); preferences(owner, true, true);
    crate::services::combat::jamming::apply(&mut s, base + 40, base + 50, base, 10000.0);
    s.think_drones();
    assert_eq!(s.drone(base + 10).unwrap().order, DroneOrder::Idle);
    s.drone_controller_lost(base);
    assert!(!s.drone_aggressors.contains_key(&owner));
    settings::logged_off(owner);
}

#[test]
fn dead_and_cloaked_aggressors_are_excluded() {
    let base = -725000; let owner = base + 1;
    settings::logged_off(owner);
    let mut s = setup(base); npc(&mut s, base + 40, 20.0); npc(&mut s, base + 41, 20.0);
    s.drone_attack(base + 40, base); s.drone_attack(base + 41, base);
    s.command(base + 40, |ball, _, _| { ball.health[2] = 0.0; Vec::new() });
    s.command(base + 41, |ball, _, _| { ball.cloak = 1; Vec::new() });
    s.tick(2000);
    preferences(owner, true, true); s.think_drones();
    assert_eq!(s.drone(base + 10).unwrap().order, DroneOrder::Idle);
    assert_eq!(s.drone(base + 11).unwrap().order, DroneOrder::Idle);
    settings::logged_off(owner);
}
