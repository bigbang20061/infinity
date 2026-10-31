use super::valid_security;

#[test]
fn security_requires_finite_inclusive_range() {
    assert!(valid_security(-10.0));
    assert!(valid_security(10.0));
    assert!(!valid_security(-10.01));
    assert!(!valid_security(10.01));
    assert!(!valid_security(f64::NAN));
    assert!(!valid_security(f64::INFINITY));
}
