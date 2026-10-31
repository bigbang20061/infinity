use super::*;

#[test]
fn optimal_and_falloff_boundaries() {
    assert_eq!(range_multiplier(100.0, 100.0, 0.0), 1.0);
    assert_eq!(range_multiplier(100.01, 100.0, 0.0), 0.0);
    assert_eq!(range_multiplier(200.0, 100.0, 100.0), 0.5);
    assert_eq!(range_multiplier(300.0, 100.0, 100.0), 0.0625);
    assert_eq!(range_multiplier(f64::NAN, 100.0, 100.0), 0.0);
}
