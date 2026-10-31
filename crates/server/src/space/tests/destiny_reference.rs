//! Native trajectory and wire-byte regression replay through `System`.
//! Expected results are committed numeric fixtures from carbonengine destiny's
//! Ballpark and Thunkers; Rust tests need neither the native runtime nor a database.

use super::*;
use std::collections::BTreeMap;
use std::fmt::Write as _;

const STREAM_TICKS: [i64; 5] = [0, 1, 10, 60, 120];

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

fn ship(velocity: V3) -> Ball {
    Ball {
        id: 1, type_id: 0, owner_id: 0, corp_id: -1, alliance_id: -1,
        char_id: None, mode: MODE_STOP, radius: 10.0, signature_radius: None,
        mass: 1_000_000.0, max_vel: 200.0, agility: 1.0, speed_fraction: 1.0,
        pos: [0.0; 3], vel: velocity, goto: [0.0; 3], follow_id: 0,
        follow_range: 0.0, free: true, global: false, massive: true,
        warp: None, warp_factor: 0, last_ms: 0, effect_stamp: 0,
        cloak: 0, cloak_until: 0, health: [1.0; DAMAGE_LAYER_COUNT],
        launcher: 0, slim: vec![],
    }
}

fn fixture(scenario: &Scenario) -> BTreeMap<i64, serde_json::Value> {
    let mut lines = scenario.fixture.lines();
    assert_eq!(lines.next(), Some(format!("# {}: carbonengine destiny reference trajectory", scenario.name).as_str()), "{}: invalid fixture header", scenario.name);
    let mut rows = BTreeMap::new();
    for line in lines {
        let row: serde_json::Value = serde_json::from_str(line).expect("invalid fixture JSON");
        let tick = row["tick"].as_i64().expect("missing fixture tick");
        let id = row["id"].as_i64().expect("missing fixture id");
        assert!((0..=120).contains(&tick) && id == 1, "{}: unexpected tick {tick} ball {id}", scenario.name);
        let stream = row.get("stream");
        assert_eq!(stream.is_some(), STREAM_TICKS.contains(&tick), "{}: invalid stream presence at tick {tick} ball {id}", scenario.name);
        if let Some(stream) = stream {
            let hex = stream.as_str().expect("stream must be a string");
            assert!(hex.len() % 2 == 0 && hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)), "{}: invalid lowercase stream at tick {tick} ball {id}", scenario.name);
        }
        assert!(rows.insert(tick, row).is_none(), "{}: duplicate tick {tick} ball {id}", scenario.name);
    }
    for tick in 0..=120 {
        assert!(rows.contains_key(&tick), "{}: missing tick {tick} ball 1", scenario.name);
    }
    rows
}

fn replay(scenario: &Scenario, streams: bool) {
    let rows = fixture(scenario);
    let mut system = System::default();
    system.insert(ship(scenario.velocity));
    for tick in 0..=120 {
        if tick == 1 {
            system.command(1, |ball, _, _| {
                ball.mode = MODE_GOTO;
                ball.goto = [1.0, 0.0, 0.0];
                vec![]
            });
        }
        if tick > 0 { system.tick(tick * 1000); }
        let id = 1;
        let ball = system.ball(id).expect("replay ball missing");
        let row = &rows[&tick];
        if streams {
            if let Some(expected) = row.get("stream") {
                let expected = expected.as_str().unwrap();
                let mut bytes = Vec::new();
                ball.write(&mut bytes);
                let mut actual = String::with_capacity(bytes.len() * 2);
                for byte in bytes { write!(&mut actual, "{byte:02x}").unwrap(); }
                if actual != expected {
                    let first = actual.as_bytes().chunks(2).zip(expected.as_bytes().chunks(2)).position(|(a, b)| a != b).unwrap_or(actual.len().min(expected.len()) / 2);
                    panic!("{} tick {tick} ball {id}: stream differs at byte {first}; actual {actual}, expected {expected}", scenario.name);
                }
            }
        } else {
            let pos = row["pos"].as_array().expect("missing fixture position");
            assert_eq!(pos.len(), 3, "{} tick {tick} ball {id}: invalid position", scenario.name);
            let expected: V3 = std::array::from_fn(|i| pos[i].as_f64().expect("invalid position component"));
            for i in 0..3 {
                let tolerance = 1e-3_f64.max(1e-6 * expected[i].abs());
                assert!((ball.pos[i] - expected[i]).abs() <= tolerance, "{} tick {tick} ball {id}: position actual {:?}, expected {:?}", scenario.name, ball.pos, expected);
            }
            assert_eq!(i64::from(ball.mode), row["mode"].as_i64().expect("missing mode"), "{} tick {tick} ball {id}: mode", scenario.name);
            assert_eq!(ball.effect_stamp, row["effectStamp"].as_i64().expect("missing effect stamp"), "{} tick {tick} ball {id}: effect stamp", scenario.name);
        }
    }
}

#[test] fn goto_trajectory() { replay(&GOTO, false); }
#[test] fn goto_stream() { replay(&GOTO, true); }
#[test] fn align_trajectory() { replay(&ALIGN, false); }
#[test] fn align_stream() { replay(&ALIGN, true); }
