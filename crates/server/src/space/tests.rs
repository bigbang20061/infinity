use super::*;
#[test]
fn seconds_to_milliseconds_preserves_fractions() {
    assert_eq!(seconds_to_ms_f64(0.0015), 1.5);
    assert_eq!(seconds_to_ms_f64(-0.0015), -1.5);
    assert_eq!(seconds_to_ms(0.0015), 1);
}


#[test]
fn lock_time_clamps_floors_and_calculates_interior() {
    assert_eq!(lock_time_ms(1.0, 1.0), 60_000);
    assert_eq!(lock_time_ms(1e9, 1e9), 500);
    assert_eq!(lock_time_ms(-10.0, 0.0), lock_time_ms(1.0, 1.0));
    let expected = (40_000.0 / (200.0 * 50.0_f64.asinh().powi(2)) * 1000.0) as u64;
    assert_eq!(lock_time_ms(200.0, 50.0), expected);
}

fn effect_stamp_balls() -> (Ball, Ball) {
    let missile = Ball {
        id: 123, type_id: 456, owner_id: 789, corp_id: 12, alliance_id: 34,
        char_id: None, mode: MODE_MISSILE, radius: 2.5, signature_radius: None,
        mass: 10.0, max_vel: 20.0, agility: 0.5, speed_fraction: 0.75,
        pos: [1.0, 2.0, 3.0], vel: [4.0, 5.0, 6.0], goto: [7.0, 8.0, 9.0],
        follow_id: 987, follow_range: 42.0, free: true, global: false, massive: false,
        warp: None, warp_factor: 3, last_ms: 1_234_567, effect_stamp: destiny_stamp(1_234_567), cloak: 0, cloak_until: 0,
        health: [1.0; DAMAGE_LAYER_COUNT], launcher: 654, slim: vec![],
    };
    let mut warp = missile.clone();
    warp.mode = MODE_WARP;
    warp.warp = Some(Warp {
        from: [1.0, 2.0, 3.0], to: [7.0, 8.0, 9.0],
        start: 2_345_678, duration: 5000.0, factor: 3,
    });
    warp.effect_stamp = destiny_stamp(2_345_678);
    (missile, warp)
}

#[test]
fn missile_and_warp_wire_bytes_remain_unchanged() {
    let (missile, warp) = effect_stamp_balls();
    for (ball, expected) in [
        (missile, "7b000000000000000500002040000000000000f03f0000000000000040000000000000084009000000000000244000ffffffffffffffff0c000000220000000000a0410000000000001040000000000000144000000000000018400000003f0000403fffdb03000000000000000028428e02000000000000d20400000000000000001c4000000000000020400000000000002240"),
        (warp, "7b000000000000000300002040000000000000f03f0000000000000040000000000000084009000000000000244000ffffffffffffffff0c000000220000000000a0410000000000001040000000000000144000000000000018400000003f0000403fff0000000000001c4000000000000020400000000000002240290900008039422edcc8244000000000000000000300000000000000"),
    ] {
        let mut bytes = Vec::new();
        ball.write(&mut bytes);
        let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(hex, expected);
    }
}

