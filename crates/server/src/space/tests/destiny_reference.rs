//! Native trajectory and wire-byte regression replay through `System`.
//! Expected results are committed numeric fixtures from carbonengine destiny's
//! Ballpark and Thunkers; Rust tests need neither the native runtime nor a database.

use super::*;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

pub(super) const STREAM_TICKS: [i64; 5] = [0, 1, 10, 60, 120];

struct Scenario {
    name: &'static str,
    fixture: &'static str,
    velocity: V3,
}

const GOTO: Scenario = Scenario {
    name: "goto-direction-rest",
    fixture: include_str!("fixtures/goto-direction-rest.jsonl"),
    velocity: [0.0, 0.0, 0.0],
};
const ALIGN: Scenario = Scenario {
    name: "align-direction-velocity",
    fixture: include_str!("fixtures/align-direction-velocity.jsonl"),
    velocity: [0.0, 200.0, 0.0],
};

pub(super) fn ship(id: i64, pos: V3, velocity: V3) -> Ball {
    Ball {
        id, type_id: 0, owner_id: 0, corp_id: -1, alliance_id: -1,
        char_id: None, mode: MODE_STOP, radius: 10.0, signature_radius: None,
        mass: 1_000_000.0, max_vel: 200.0, agility: 1.0, speed_fraction: 1.0,
        pos, vel: velocity, goto: [0.0; 3], follow_id: 0,
        old_pos: pos, old_vel: velocity, segment_target: None, segment_warp: None,
        follow_range: 0.0, free: true, global: false, massive: true, interactive: true,
        warp: None, warp_factor: 0, last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0,
        cloak: 0, cloak_until: 0, health: [1.0; DAMAGE_LAYER_COUNT],
        launcher: 0, slim: vec![],
    }
}

fn fixture(name: &str, fixture: &str, ids: &BTreeSet<i64>, stream_ticks: &[i64]) -> BTreeMap<(i64, i64), serde_json::Value> {
    let mut lines = fixture.lines();
    assert_eq!(lines.next(), Some(format!("# {name}: carbonengine destiny reference trajectory").as_str()), "{name}: invalid fixture header");
    let mut rows = BTreeMap::new();
    for line in lines {
        let row: serde_json::Value = serde_json::from_str(line).expect("invalid fixture JSON");
        let tick = row["tick"].as_i64().expect("missing fixture tick");
        let id = row["id"].as_i64().expect("missing fixture id");
        assert!((0..=120).contains(&tick) && ids.contains(&id), "{name}: unexpected tick {tick} ball {id}");
        let stream = row.get("stream");
        assert_eq!(stream.is_some(), stream_ticks.contains(&tick), "{name}: invalid stream presence at tick {tick} ball {id}");
        if let Some(stream) = stream {
            let hex = stream.as_str().expect("stream must be a string");
            assert!(hex.len() % 2 == 0 && hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)), "{name}: invalid lowercase stream at tick {tick} ball {id}");
        }
        for field in ["pos", "vel"] {
            let vector = row[field].as_array().expect("missing fixture vector");
            assert_eq!(vector.len(), 3, "{name}: invalid {field} at tick {tick} ball {id}");
            assert!(vector.iter().all(|n| n.as_f64().is_some_and(f64::is_finite)), "{name}: invalid {field} at tick {tick} ball {id}");
        }
        assert!(row["speedFraction"].as_f64().is_some_and(f64::is_finite));
        assert!(row["mode"].as_i64().is_some() && row["effectStamp"].as_i64().is_some());
        assert!(rows.insert((tick, id), row).is_none(), "{name}: duplicate tick {tick} ball {id}");
    }
    for tick in 0..=120 {
        for id in ids {
            assert!(rows.contains_key(&(tick, *id)), "{name}: missing tick {tick} ball {id}");
        }
    }
    rows
}

fn start_direction(system: &mut System) {
    system.command(1, |ball, _, _| {
        ball.goto_direction([1.0, 0.0, 0.0]);
        vec![]
    });
}

fn compare_position(actual: V3, expected: V3, context: &str) {
    for i in 0..3 {
        let tolerance = 1e-3_f64.max(1e-6 * expected[i].abs());
        assert!((actual[i] - expected[i]).abs() <= tolerance, "{context}: position actual {actual:?}, expected {expected:?}");
    }
}

pub(super) fn replay(name: &str, fixture_data: &str, balls: Vec<Ball>, stream_ticks: &[i64], streams: bool, setup: impl FnOnce(&mut System)) {
    let ids: BTreeSet<_> = balls.iter().map(|ball| ball.id).collect();
    assert_eq!(ids.len(), balls.len(), "{name}: duplicate initial ball IDs");
    let rows = fixture(name, fixture_data, &ids, stream_ticks);
    let mut system = System::default();
    for ball in balls { system.insert(ball); }
    let mut setup = Some(setup);
    for tick in 0..=120 {
        if tick == 1 { setup.take().unwrap()(&mut system); }
        if tick > 0 { system.tick(tick * 1000); }
        for id in &ids {
            let ball = system.ball(*id).expect("replay ball missing");
            let row = &rows[&(tick, *id)];
            if streams {
                if let Some(expected) = row.get("stream") {
                    let expected = expected.as_str().unwrap();
                    let mut bytes = Vec::new();
                    ball.write(&mut bytes);
                    let mut actual = String::with_capacity(bytes.len() * 2);
                    for byte in bytes { write!(&mut actual, "{byte:02x}").unwrap(); }
                    if actual != expected {
                        let first = actual.as_bytes().chunks(2).zip(expected.as_bytes().chunks(2)).position(|(a, b)| a != b).unwrap_or(actual.len().min(expected.len()) / 2);
                        panic!("{name} tick {tick} ball {id}: stream differs at byte {first}; actual {actual}, expected {expected}");
                    }
                }
            } else {
                let pos = row["pos"].as_array().unwrap();
                let expected = std::array::from_fn(|i| pos[i].as_f64().unwrap());
                compare_position(ball.pos, expected, &format!("{name} tick {tick} ball {id}"));
                assert_eq!(i64::from(ball.mode), row["mode"].as_i64().unwrap(), "{name} tick {tick} ball {id}: mode");
                assert_eq!(ball.effect_stamp, row["effectStamp"].as_i64().unwrap(), "{name} tick {tick} ball {id}: effect stamp");
            }
        }
    }
}

fn replay_scenario(scenario: &Scenario, streams: bool) {
    replay(scenario.name, scenario.fixture, vec![ship(1, [0.0; 3], scenario.velocity)], &STREAM_TICKS, streams, start_direction);
}

#[test] fn goto_trajectory() { replay_scenario(&GOTO, false); }
#[test] fn goto_stream() { replay_scenario(&GOTO, true); }
#[test] fn align_trajectory() { replay_scenario(&ALIGN, false); }
#[test] fn align_stream() { replay_scenario(&ALIGN, true); }

fn replay_fractions(scenario: &Scenario, fixture: &str) {
    let mut lines = fixture.lines();
    assert_eq!(lines.next(), Some(format!("# {}-fractions: carbonengine destiny reference interpolation", scenario.name).as_str()), "{}: invalid fraction fixture header", scenario.name);
    let mut system = System::default();
    system.insert(ship(1, [0.0; 3], scenario.velocity));
    start_direction(&mut system);
    let fractions = [(0.0, 0), (0.1, 100), (0.5, 500), (0.9, 900), (1.0, 1000)];
    for tick in 1..=120 {
        let row: serde_json::Value = serde_json::from_str(lines.next().expect("missing fraction fixture row")).expect("invalid fraction fixture JSON");
        assert_eq!(row["tick"].as_i64(), Some(tick), "{}: invalid fraction tick", scenario.name);
        assert_eq!(row["id"].as_i64(), Some(1), "{} tick {tick}: invalid ball id", scenario.name);
        let samples = row["samples"].as_array().expect("missing fraction samples");
        assert_eq!(samples.len(), fractions.len(), "{} tick {tick}: invalid sample count", scenario.name);
        let previous = system.ball(1).expect("replay ball missing").clone();
        system.tick(tick * 1000);
        let ball = system.ball(1).expect("replay ball missing");
        assert_eq!(ball.old_pos, previous.pos, "{} tick {tick}: segment start position", scenario.name);
        assert_eq!(ball.old_vel, previous.vel, "{} tick {tick}: segment start velocity", scenario.name);
        for (sample, (fraction, offset)) in samples.iter().zip(fractions) {
            assert_eq!(sample["fraction"].as_f64(), Some(fraction), "{} tick {tick}: invalid fraction order", scenario.name);
            let position = sample["pos"].as_array().expect("missing fraction position");
            let velocity = sample["vel"].as_array().expect("missing fraction velocity");
            assert_eq!(position.len(), 3, "{} tick {tick}: invalid position length", scenario.name);
            assert_eq!(velocity.len(), 3, "{} tick {tick}: invalid velocity length", scenario.name);
            let expected: V3 = std::array::from_fn(|i| position[i].as_f64().filter(|v| v.is_finite()).expect("invalid position component"));
            for component in velocity {
                assert!(component.as_f64().is_some_and(f64::is_finite), "{} tick {tick}: invalid velocity component", scenario.name);
            }
            let actual = ball.position_at(tick * 1000 + offset);
            compare_position(actual, expected, &format!("{} tick {tick} fraction {fraction} ball 1", scenario.name));
        }
    }
    assert!(lines.next().is_none(), "{}: unexpected fraction fixture rows", scenario.name);
}

#[test] fn goto_fractions() {
    replay_fractions(&GOTO, include_str!("fixtures/goto-direction-rest-fractions.jsonl"));
}
#[test] fn align_fractions() {
    replay_fractions(&ALIGN, include_str!("fixtures/align-direction-velocity-fractions.jsonl"));
}
