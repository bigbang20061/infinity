use super::distance;

#[test]
fn distance_rejects_nonfinite_and_negative_values() {
    assert_eq!(distance(None, 5000.0), Some(5000.0));
    assert_eq!(distance(Some(&"0".to_string()), 1.0), Some(0.0));
    assert_eq!(distance(Some(&"-1".to_string()), 1.0), None);
    assert_eq!(distance(Some(&"NaN".to_string()), 1.0), None);
    assert_eq!(distance(Some(&"inf".to_string()), 1.0), None);
}
