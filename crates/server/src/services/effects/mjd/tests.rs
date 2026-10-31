use super::*;

#[test]
fn absent_type_row_uses_client_distance_without_evaluating_fit() {
    assert_eq!(resolve_range(None, || panic!("fit must not be evaluated")), Some(100_000.0));
}

#[test]
fn present_type_row_uses_fitted_range() {
    assert_eq!(resolve_range(Some(120_000.0), || 150_000.0), Some(150_000.0));
    assert_eq!(resolve_range(Some(0.0), || 25_000.0), Some(25_000.0));
}

#[test]
fn present_type_row_rejects_invalid_fitted_range() {
    for value in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(resolve_range(Some(100_000.0), || value), None);
    }
}
