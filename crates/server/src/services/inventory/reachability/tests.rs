use super::in_range;

#[test]
fn range_uses_missing_distance_fallback_and_inclusive_limit() {
    assert!(in_range(Some(2500.0), None));
    assert!(!in_range(Some(2500.0001), None));
    assert!(in_range(Some(0.0), Some(0.0)));
    assert!(!in_range(Some(0.0001), Some(0.0)));
    assert!(in_range(Some(42.0), Some(42.0)));
    assert!(!in_range(Some(42.0001), Some(42.0)));
    assert!(!in_range(None, None));
}

#[test]
fn range_rejects_nonfinite_geometry_and_limits() {
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(!in_range(Some(value), None));
        assert!(!in_range(Some(0.0), Some(value)));
    }
}
