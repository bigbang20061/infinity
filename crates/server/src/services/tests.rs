use super::*;

#[test]
fn surface_offset_includes_radius() {
    let center = [1.0e9, -2.0e9, 3.0e9];
    for radius in [0.0, 6_000_000.0, 1.0e12] {
        let clearance = 1_000_000.0;
        let actual = crate::space::dist(center, off_surface(center, radius, clearance));
        let expected = radius + clearance;
        assert!((actual - expected).abs() < expected * 1e-12, "{actual} != {expected}");
    }
}
