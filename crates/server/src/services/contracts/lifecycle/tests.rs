use super::sum_volume;

#[test]
fn volume_sum_preserves_sql_zero_and_nonempty_values() {
    assert_eq!(sum_volume(std::iter::empty()).to_bits(), 0.0f64.to_bits());
    assert_eq!(sum_volume([1.25, 2.5, 4.0].into_iter()).to_bits(), 7.75f64.to_bits());
}
