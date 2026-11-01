//! Pure tests for client rack evolution and the published damage policy.
//! Curve behavior mirrors `dogma/attributes/heatAttribute.py`.
//! Inputs are fixed hand-computed values rather than database or client data.
//! Actor timing, notification delivery and durable damage are not exercised here.

use super::rack::{curve, damage_chance};

#[test]
fn fixed_curve_references() {
    assert_eq!(curve(20.0, 100.0, 0.0, 1.0, 0.01, 1000.0).to_bits(), 0x4033cd0e1e3b421f);
    assert_eq!(curve(20.0, 100.0, 0.5, 0.5, 0.01, 1000.0).to_bits(), 0x4042d91479a85d72);
}

#[test]
fn cooling_cutoff_and_epsilon() {
    assert_eq!(curve(0.5, 100.0, 0.0, 1.0, 0.0, 0.0), 0.5);
    assert_eq!(curve(f64::from_bits(0.5_f64.to_bits() - 1), 100.0, 0.0, 1.0, 0.0, 0.0), 0.0);
    assert_eq!(curve(20.0, 100.0, 5e-8 - f64::EPSILON, 1.0, 0.01, 1000.0).to_bits(), 0x4033cd0e1e3b421f);
    assert!(curve(20.0, 100.0, 5e-8, 1.0, 0.01, 1000.0) > 20.0);
}

#[test]
fn curve_monotonic_and_bounded() {
    let a = curve(20.0, 100.0, 0.5, 0.5, 0.01, 1000.0);
    let b = curve(a, 100.0, 0.5, 0.5, 0.01, 1000.0);
    assert!(20.0 < a && a < b && b < 100.0);
    assert_eq!(curve(100.0, 100.0, 0.5, 0.5, 0.01, 1000.0), 100.0);
    assert!(curve(20.0, 100.0, 0.0, 1.0, 0.01, 2000.0) < 20.0);
}

#[test]
fn published_chance_and_physical_distance() {
    let chance = damage_chance(60.0, 100.0, 11, 14, 0.5, 0, 0, true);
    assert_eq!(chance.to_bits(), 0.4714285714285714_f64.to_bits());
    assert_eq!(damage_chance(60.0, 100.0, 11, 14, 0.5, 0, 1, true), chance * 0.5);
    assert_eq!(damage_chance(60.0, 100.0, 11, 14, 0.5, 0, 1, false), 0.0);
    assert_eq!(damage_chance(60.0, 100.0, 11, 14, 0.5, 0, 2, true), chance * 0.25);
    assert_eq!(damage_chance(60.0, 0.0, 11, 14, 0.5, 0, 0, true), 0.0);
    assert_eq!(damage_chance(60.0, 100.0, 11, 0, 0.5, 0, 0, true), 0.0);
}


mod early_clear;
