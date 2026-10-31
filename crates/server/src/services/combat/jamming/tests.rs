use super::*;
use crate::space::{Ball, System, WarpState, MODE_STOP};
use std::sync::Arc;

#[test]
fn jam_clears_locks_and_overlaps_expire_independently() {
    crate::tests::init_config();
    let ship = NEXT_LOCK.fetch_add(10, Ordering::Relaxed) as i64;
    let pilot = ship + 1;
    let source = ship + 2;
    let module = ship + 3;
    let other = ship + 4;
    let locked = ship + 5;
    let pending = ship + 6;
    let mut system = System::new(ship, Arc::default(), 1000);
    system.insert(Ball {
        id: ship, type_id: 1, owner_id: pilot, corp_id: 1, alliance_id: 1,
        char_id: Some(pilot), signature_radius: None, mode: MODE_STOP, radius: 1.0,
        mass: 1.0, max_vel: 0.0, agility: 1.0, speed_fraction: 0.0,
        pos: [0.0; 3], vel: [0.0; 3], old_pos: [0.0; 3], old_vel: [0.0; 3],
        segment_acceleration: None, goto: [0.0; 3], follow_id: 0, follow_range: 0.0,
        free: false, global: false, massive: true, interactive: true,
        warp: WarpState::default(), warp_factor: 0, last_ms: 0, interpolation_anchor_ms: 0,
        effect_stamp: 0, cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new(),
    });
    {
        let mut st = state();
        st.targets.entry(pilot).or_default().insert(locked);
        st.locking.insert((pilot, pending), 1);
    }
    apply(&mut system, source, module, ship, 100.0);
    assert!(is_jammed(ship, 1000));
    assert!(!state().targets.contains_key(&pilot));
    {
        let mut st = state();
        assert!(!st.begin_lock(ship, pilot, pending, 2, 1000));
        assert_eq!(st.complete_lock(ship, pilot, pending, 1, 1000, true), (false, false));
        st.locking.insert((pilot, pending), 3);
        assert_eq!(st.complete_lock(ship, pilot, pending, 3, 1000, true), (true, false));
        assert!(!st.targets.contains_key(&pilot));
    }
    assert!(!state().locking.contains_key(&(pilot, pending)));
    apply(&mut system, other, module, ship, 300.0);
    apply(&mut system, source, module, ship, 200.0);
    system.tick(1100);
    assert!(is_jammed(ship, 1100));
    assert_eq!(state().jammed[&ship].len(), 2);
    system.tick(1200);
    assert!(is_jammed(ship, 1200));
    assert_eq!(state().jammed[&ship].len(), 1);
    system.tick(1300);
    assert!(!is_jammed(ship, 1300));
    assert!(!state().jammed.contains_key(&ship));
}
