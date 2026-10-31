use super::parse;

#[test]
fn damage_arguments_validate_layers_and_finite_percent() {
    let args = |a: &str, b: &str| vec![a.to_owned(), b.to_owned()];
    assert_eq!(parse(&args("armor", "25")), Some(([false, true, false], 0.25)));
    assert_eq!(parse(&args("all", "100")), Some(([true; 3], 1.0)));
    assert!(parse(&args("hull", "NaN")).is_none());
    assert!(parse(&args("shield", "101")).is_none());
    assert!(parse(&args("bad", "0")).is_none());
}
