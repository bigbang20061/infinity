use super::*;

fn changed(s: &System) -> Vec<Position> {
    s.pilots().filter_map(|ball| {
        let &(generation, previous) = s.snapshot_positions.get(&ball.id)?;
        (ball.pos != previous).then_some(Position { id: ball.id, generation, pos: ball.pos })
    }).collect()
}

fn ball(id: i64, pilot: Option<i64>, x: f64) -> Ball {
    Ball {
        id, type_id: 1, owner_id: 1, corp_id: 1, alliance_id: -1,
        char_id: pilot, signature_radius: None, mode: MODE_STOP, radius: 1.0,
        mass: 1e6, max_vel: 100.0, agility: 1.0, speed_fraction: 0.0,
        pos: [x, 0.0, 0.0], vel: [0.0; 3], goto: [0.0; 3], follow_id: 0,
        follow_range: 0.0, free: true, global: false, massive: true, warp: None,
        warp_factor: 0, last_ms: 0, effect_stamp: 0, cloak: 0, cloak_until: 0,
        health: [1.0; 3], launcher: 0, slim: Vec::new(),
    }
}

#[test]
fn candidates_only_include_changed_pilot_ids_and_generations() {
    let mut s = System::default();
    s.insert(ball(1, Some(10), 0.0));
    s.insert(ball(2, Some(20), 0.0));
    s.insert(ball(3, None, 0.0));
    assert!(candidates(&s).next().is_none());
    s.command(1, |b, _, _| { b.pos[0] = 5.0; Vec::new() });
    s.command(3, |b, _, _| { b.pos[0] = 5.0; Vec::new() });
    let generation = s.snapshot_positions[&1].0;
    assert_eq!(candidates(&s).collect::<Vec<_>>(), vec![(1, generation)]);
}

#[test]
fn changed_pilots_retry_conflicts_and_acknowledge_only_written_positions() {
    let mut s = System::default();
    s.insert(ball(1, Some(10), 0.0));
    s.insert(ball(2, Some(20), 0.0));
    s.insert(ball(3, None, 0.0));
    assert!(changed(&s).is_empty());
    for id in 1..=3 {
        s.command(id, |b, _, _| { b.pos[0] = 5.0; Vec::new() });
    }
    let positions = changed(&s);
    assert_eq!(positions.len(), 2);
    acknowledge(&mut s, positions, &[1].into_iter().collect());
    let remaining = changed(&s);
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].id, 2);
    acknowledge(&mut s, remaining, &[2].into_iter().collect());
    assert!(changed(&s).is_empty());
}

#[test]
fn old_acknowledgement_cannot_mark_reinserted_ship_written() {
    let mut s = System::default();
    s.insert(ball(1, Some(10), 0.0));
    s.command(1, |b, _, _| { b.pos[0] = 5.0; Vec::new() });
    let old = changed(&s);
    s.take(1);
    s.insert(ball(1, Some(10), 0.0));
    s.command(1, |b, _, _| { b.pos[0] = 5.0; Vec::new() });
    acknowledge(&mut s, old, &[1].into_iter().collect());
    assert_eq!(changed(&s).len(), 1);
}

#[test]
fn no_pilots_keep_cold_skip_and_no_deadline() {
    let mut s = System::default();
    s.insert(ball(1, None, 0.0));
    s.tick(POSITION_SNAPSHOT_INTERVAL_MS as i64);
    assert!(s.is_cold());
    assert_eq!(s.snapshot_due, 0);
    assert!(changed(&s).is_empty());
}
