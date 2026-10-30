use super::*;
#[test]
fn seconds_to_milliseconds_preserves_fractions() {
    assert_eq!(seconds_to_ms_f64(0.0015), 1.5);
    assert_eq!(seconds_to_ms_f64(-0.0015), -1.5);
    assert_eq!(seconds_to_ms(0.0015), 1);
}


#[test]
fn lock_time_clamps_floors_and_calculates_interior() {
    assert_eq!(lock_time_ms(1.0, 1.0), 60_000);
    assert_eq!(lock_time_ms(1e9, 1e9), 500);
    assert_eq!(lock_time_ms(-10.0, 0.0), lock_time_ms(1.0, 1.0));
    let expected = (40_000.0 / (200.0 * 50.0_f64.asinh().powi(2)) * 1000.0) as u64;
    assert_eq!(lock_time_ms(200.0, 50.0), expected);
}
