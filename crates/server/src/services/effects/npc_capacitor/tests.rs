use super::*;

#[test]
fn amount_respects_falloff_and_invalid_input() {
    assert_eq!(scaled_amount(100.0, 10.0, 10.0, 20.0), 100.0);
    assert_eq!(scaled_amount(100.0, 30.0, 10.0, 20.0), 50.0);
    assert_eq!(scaled_amount(100.0, 11.0, 10.0, 0.0), 0.0);
    assert_eq!(scaled_amount(-1.0, 0.0, 10.0, 0.0), 0.0);
    assert_eq!(scaled_amount(f64::NAN, 0.0, 10.0, 0.0), 0.0);
}
