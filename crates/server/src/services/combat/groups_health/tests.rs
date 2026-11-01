//! Health and capacitor boundary tests without a database or running system actor.
use super::*;
use crate::dao::items::creation;

mod db_smoke;
mod fixture;

/// Drain up to the requested amount for capacitor integration tests.
pub(in crate::services) async fn drain_exact(db: &Db, world: &World, system: i64, ship: i64, amount: f64) -> Result<f64, CallError> {
    Ok(capacitor_operation(db, world, system, ship, Operation::Drain(amount), None).await?.map_or(0.0, |(_, amount, _)| amount))
}

#[test]
fn recharge_uses_existing_curve_and_never_runs_backwards() {
    let level = recharge(25.0, 100.0, 1000.0, 1.0);
    assert!((level - 100.0 * (1.0 - 0.5 * (-5.0_f64).exp()).powi(2)).abs() < 1e-9);
    assert_eq!(recharge(25.0, 100.0, 1000.0, -1.0), 25.0);
}

#[test]
fn injection_and_capacity_are_clamped() {
    let mut entries = HashMap::new();
    entries.insert(1, (80.0, 0));
    let result = mutate(&mut entries, 1, 100.0, 1000.0, 0, Operation::Inject(50.0));
    assert_eq!((result.after, result.amount), (100.0, 20.0));
    let result = mutate(&mut entries, 1, 50.0, 1000.0, 0, Operation::Inject(-5.0));
    assert_eq!((result.after, result.amount), (50.0, 0.0));
    let result = mutate(&mut entries, 1, -1.0, 1000.0, 0, Operation::Snapshot);
    assert_eq!(result.after, 0.0);
}

#[test]
fn drone_assistance_rejects_missing_or_self_source() {
    let system = crate::space::System::new(1, std::sync::Arc::default(), 0);
    for target in [1, 2] {
        assert!(!NpcCapacitorGuard::DroneAssistance {
            source: 1, target, generation: 3, optimal: 100.0, falloff: 0.0, owner: 4,
        }.valid(&system));
    }
}

#[test]
fn drain_is_partial_and_negative_input_cannot_add_energy() {
    let mut entries = HashMap::new();
    entries.insert(1, (30.0, 0));
    let result = mutate(&mut entries, 1, 100.0, 1000.0, 0, Operation::Drain(50.0));
    assert_eq!(result.after, 0.0);
    // The recharge curve's sqrt/square round trip can introduce rounding at dt = 0.
    assert!((result.amount - 30.0).abs() < 1e-12);
    let result = mutate(&mut entries, 1, 100.0, 1000.0, 0, Operation::Drain(-10.0));
    assert_eq!((result.after, result.amount), (0.0, 0.0));
}

#[test]
fn refused_cycle_drain_leaves_stored_state_untouched() {
    let mut entries = HashMap::new();
    entries.insert(1, (20.0, 0));
    let result = mutate(&mut entries, 1, 100.0, 1000.0, 0, Operation::RefuseDrain(30.0));
    assert_eq!(result.amount, 0.0);
    assert_eq!(entries.get(&1), Some(&(20.0, 0)));
}

#[test]
fn npc_repairs_are_layer_isolated_bounded_and_never_resurrect() {
    let mut hp = Hp { now: [20.0, 30.0, 40.0], max: [100.0; 3], resonance: [[1.0; 4]; 3] };
    assert_eq!(repair_npc_hp(&mut hp, 1, 25.0), Some([0.2, 0.55, 0.4]));
    assert_eq!(hp.now, [20.0, 55.0, 40.0]);
    assert_eq!(repair_npc_hp(&mut hp, 1, f64::MAX), Some([0.2, 1.0, 0.4]));
    for (layer, amount) in [(3, 1.0), (0, -1.0), (0, f64::NAN), (0, f64::INFINITY), (1, 1.0)] {
        assert!(repair_npc_hp(&mut hp, layer, amount).is_none());
    }
    assert_eq!(hp.now, [20.0, 100.0, 40.0]);
    hp.now[2] = 0.0;
    assert!(repair_npc_hp(&mut hp, 2, 50.0).is_none());
    assert_eq!(hp.now[2], 0.0);
}

#[test]
fn npc_fraction_uses_cached_layer_and_defaults_to_full() {
    let ball = Ball {
        id: -8_736_421, type_id: 1, owner_id: 1, corp_id: 1, alliance_id: -1,
        char_id: None, signature_radius: None, mode: crate::space::MODE_STOP,
        radius: 1.0, mass: 1.0, max_vel: 0.0, agility: 1.0, speed_fraction: 0.0,
        pos: [0.0; 3], vel: [0.0; 3], old_pos: [0.0; 3], old_vel: [0.0; 3],
        segment_acceleration: None, goto: [0.0; 3], follow_id: 0, follow_range: 0.0,
        free: true, global: false, massive: true, interactive: false,
        warp: crate::space::WarpState::default(), warp_factor: 0, last_ms: 0,
        interpolation_anchor_ms: 0, effect_stamp: 0, cloak: 0, cloak_until: 0,
        health: [1.0; 3], launcher: 0, slim: Vec::new(),
    };
    assert_eq!(npc_health_fraction(&ball, 0), Some(1.0));
    assert_eq!(npc_health_fraction(&ball, 3), None);
    state().hp.insert(ball.id, Hp { now: [25.0, 150.0, 50.0], max: [100.0; 3], resonance: [[1.0; 4]; 3] });
    assert_eq!(npc_health_fraction(&ball, 0), Some(0.25));
    assert_eq!(npc_health_fraction(&ball, 1), Some(1.0));
    state().hp.remove(&ball.id);
}

#[test]
fn concurrent_npc_damage_does_not_overwrite_cached_updates() {
    let target = i64::MIN + 205;
    state().hp.remove(&target);
    std::thread::scope(|scope| {
        for _ in 0..2 {
            scope.spawn(|| {
                for _ in 0..100 {
                    damage_npc_cached(target, || Hp {
                        now: [1000.0; 3], max: [1000.0; 3], resonance: [[1.0; 4]; 3],
                    }, [1.0, 0.0, 0.0, 0.0]);
                }
            });
        }
    });
    assert_eq!(state().hp.remove(&target).unwrap().now[0], 800.0);
}

#[test]
fn drone_hp_rollback_restores_absence_and_exact_previous_value() {
    let target = i64::MIN + 206;
    for damaged in [[80.0, 100.0, 100.0], [0.0; 3]] {
        for existed in [false, true] {
            let original = Hp { now: [90.0, 80.0, 70.0], max: [100.0; 3], resonance: [[0.5; 4]; 3] };
            let previous = existed.then(|| original.clone());
            state().hp.insert(target, Hp { now: damaged, max: [100.0; 3], resonance: [[1.0; 4]; 3] });
            restore_drone_hp(target, previous);
            let restored = state().hp.remove(&target);
            if existed {
                let restored = restored.unwrap();
                assert_eq!((restored.now, restored.max, restored.resonance), (original.now, original.max, original.resonance));
            } else {
                assert!(restored.is_none());
            }
        }
    }
}
