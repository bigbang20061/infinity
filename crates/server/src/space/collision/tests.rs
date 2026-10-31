//! Focused boundaries for the native sphere collision mechanic.
use super::*;

fn sphere(id: i64, pos: V3, velocity: V3) -> Snapshot {
    let ball = super::super::tests::destiny_reference::ship(id, pos, velocity);
    Snapshot::new(&ball)
}

#[test]
fn stationary_contact_wakes_and_disabled_targets_do_not() {
    let mut system = system::System::new(0, std::sync::Arc::default(), 0);
    let a = super::super::tests::destiny_reference::ship(1, [0.0; 3], [0.0; 3]);
    let mut b = super::super::tests::destiny_reference::ship(2, [20.0, 0.0, 0.0], [0.0; 3]);
    b.free = false;
    b.massive = false;
    system.insert(a);
    system.insert(b.clone());
    assert!(system.is_cold());
    b.massive = true;
    b.cloak = 1;
    system.insert(b.clone());
    assert!(system.is_cold());
    b.cloak = 0;
    system.insert(b);
    assert!(!system.is_cold());
    system.tick(1000);
    assert_ne!(system.ball(1).unwrap().pos, [0.0; 3]);
}

#[test]
fn sweep_tangent_miss_and_fast_crossing() {
    let zero = [0.0; 3];
    assert_eq!(collide_two_spheres([-2.0, 1.0, 0.0], [2.0, 1.0, 0.0], zero, zero, 1.0), Some(0.5));
    assert_eq!(collide_two_spheres([-2.0, 1.1, 0.0], [2.0, 1.1, 0.0], zero, zero, 1.0), None);
    assert_eq!(collide_two_spheres([-10.0, 0.0, 0.0], [10.0, 0.0, 0.0], zero, zero, 1.0), Some(0.45));
    assert!(quadratic(1.0, 0.0, 1.0).is_none());
}

#[test]
fn broadphase_keeps_boundaries_and_uncertainty() {
    let zero = [0.0; 3];
    assert!(swept_aabb(zero, zero, [2.0, 0.0, 0.0], [2.0, 0.0, 0.0], 2.0));
    assert!(!swept_aabb(zero, zero, [3.0, 0.0, 0.0], [3.0, 0.0, 0.0], 2.0));
    assert!(swept_aabb([f64::NAN, 0.0, 0.0], zero, [3.0, 0.0, 0.0], [3.0, 0.0, 0.0], 2.0));
}

#[test]
fn source_cloak_is_asymmetric_and_nonmassive_excluded() {
    let mut a = sphere(1, [0.0; 3], [0.0; 3]);
    let mut b = sphere(2, [20.0, 0.0, 0.0], [0.0; 3]);
    a.cloak = 1;
    assert!(eligible(&a, &b));
    assert!(!eligible(&b, &a));
    b.massive = false;
    assert!(!eligible(&a, &b));
    assert!(!eligible(&b, &a));
}

#[test]
fn latest_and_equal_strongest_fold_is_deterministic() {
    let me = sphere(1, [0.0; 3], [100.0, 0.0, 0.0]);
    let mut first = sphere(2, [30.0, 0.0, 0.0], [0.0; 3]);
    let mut later = sphere(3, [50.0, 0.0, 0.0], [0.0; 3]);
    first.free = false;
    later.free = false;
    let all = gradient(&me, &[me, first, later]);
    assert_eq!(all, gradient(&me, &[me, later]));
    assert_eq!(all, gradient(&me, &[later, first, me]));
    let mut state = Fold { collision: 0.0, acceleration: [1.0, 0.0, 0.0] };
    let overlap = sphere(4, [10.0, 0.0, 0.0], [0.0; 3]);
    potential(me, overlap, 0, &mut state);
    assert!(dot(state.acceleration, state.acceleration) > 1.0);
}
