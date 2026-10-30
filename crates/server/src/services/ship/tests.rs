use super::{gate_radius, DEFAULT_CELESTIAL_RADIUS};

#[test]
fn destination_gate_radius_preserves_sql_coalesce_order() {
    assert_eq!(gate_radius(Some(0.0), Some(7.0)), 0.0);
    assert_eq!(gate_radius(None, Some(7.0)), 7.0);
    assert_eq!(gate_radius(None, None), DEFAULT_CELESTIAL_RADIUS);
}
