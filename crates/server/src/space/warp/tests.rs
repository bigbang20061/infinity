use super::*;

fn ball() -> Ball {
    Ball {
        id: 1, type_id: 1, owner_id: 1, corp_id: 1, alliance_id: -1, char_id: None,
        mode: MODE_STOP, radius: 1.0, signature_radius: None, mass: 1e6,
        max_vel: 100.0, agility: 1.0, speed_fraction: 1.0, pos: [0.0; 3], vel: [0.0; 3],
        old_pos: [0.0; 3], old_vel: [0.0; 3], segment_acceleration: None,
        goto: [0.0; 3], follow_id: 0, follow_range: 0.0, free: true, global: false,
        massive: true, interactive: false, warp: WarpState::default(), warp_factor: 3000,
        last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0, cloak: 0, cloak_until: 0,
        health: [1.0; 3], launcher: 0, slim: Vec::new(),
    }
}

#[test]
fn alignment_preserves_counter_and_activates_with_range() {
    let mut b = ball();
    let target = [1_000_000.0, 0.0, 0.0];
    assert_eq!(b.warp_to(target, 1000.0, 3000), WarpOutcome::Align);
    assert_eq!(b.warp.length, -1.0);
    b.evolve_warp(0);
    assert_eq!(b.effect_stamp, -2);
    assert_eq!(b.warp_to(target, 500.0, 1), WarpOutcome::Ignored);
    assert_eq!(b.effect_stamp, -2);
    b.vel = [100.0, 0.0, 0.0];
    b.evolve_warp(1);
    assert!(b.is_warping());
    assert_eq!(b.goto, [999_000.0, 0.0, 0.0]);
    assert_eq!(b.warp.length, 999_000.0);
    assert_eq!(b.warp.min_range, 1000.0);
    assert!(!b.massive);
    b.stop();
    assert_eq!(b.warp_to([2_000_000.0, 0.0, 0.0], -1.0, 0), WarpOutcome::Align);
    assert_eq!(b.warp.length, 999_000.0);
    assert_eq!(b.warp.min_range, 0.0);
    assert_eq!(b.warp.factor, 1);
}

#[test]
fn strict_speed_and_forced_alignment_thresholds() {
    let mut b = ball();
    b.warp_to([1_000_000.0, 0.0, 0.0], 0.0, 3000);
    b.vel = [75.0, 0.0, 0.0];
    assert!(!b.is_aligned_for_warp());
    b.vel[0] = 75.001;
    assert!(b.is_aligned_for_warp());
    b.vel = [0.0; 3];
    b.effect_stamp = -180;
    assert!(!b.is_aligned_for_warp());
    b.effect_stamp = -181;
    assert!(b.is_aligned_for_warp());
}

#[test]
fn short_and_invalid_requests_keep_native_semantics() {
    let mut b = ball();
    b.speed_fraction = 0.5;
    assert_eq!(b.warp_to([99_999.0, 0.0, 0.0], 0.0, 3000), WarpOutcome::Approach);
    assert_eq!(b.mode, MODE_GOTO);
    assert_eq!(b.speed_fraction, 0.5);
    assert_eq!(b.warp_to([f64::NAN, 0.0, 0.0], 0.0, 3000), WarpOutcome::Ignored);
    assert_eq!(b.mode, MODE_GOTO);
}
