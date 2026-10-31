use super::{job_deadline, job_seconds, FILETIME_SECOND};

#[test]
fn duration_multiplier_applies_before_rounding_and_duration_guard() {
    assert_eq!(job_seconds(10.5, 0.5, 1, 100.0).ok(), Some(5));
    assert_eq!(job_seconds(10.5, 1.0, 1, 100.0).ok(), Some(11));
    assert_eq!(job_seconds(10.5, 2.0, 1, 100.0).ok(), Some(21));
    assert!(job_seconds(100.0, 2.0, 2, 75.0).is_err());
    assert!(job_seconds(100.0, 0.5, 2, 75.0).is_ok());
}

#[test]
fn duration_rejects_nonfinite_and_unrepresentable_results() {
    assert!(job_seconds(f64::MAX, 2.0, 1, f64::MAX).is_err());
    assert!(job_seconds(i64::MAX as f64, 1.0, 1, f64::MAX).is_err());
    assert!(job_seconds(f64::NAN, 1.0, 1, f64::MAX).is_err());
}

#[test]
fn deadline_adds_duration_to_start() {
    assert_eq!(job_deadline(123, 2).ok(), Some(123 + 2 * FILETIME_SECOND));
}

#[test]
fn deadline_rejects_duration_multiplication_overflow() {
    assert!(job_deadline(0, i64::MAX / FILETIME_SECOND + 1).is_err());
}

#[test]
fn deadline_rejects_timestamp_addition_overflow() {
    assert!(job_deadline(i64::MAX, 1).is_err());
}
