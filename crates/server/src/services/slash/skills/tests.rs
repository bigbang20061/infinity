use super::*;

#[test]
fn skill_levels_default_and_validate_boundaries() {
    assert!(matches!(level(None), Ok(5)));
    assert!(matches!(level(Some("0")), Ok(0)));
    assert!(matches!(level(Some("5")), Ok(5)));
    assert!(level(Some("-1")).is_err());
    assert!(level(Some("6")).is_err());
    assert!(level(Some("two")).is_err());
}
