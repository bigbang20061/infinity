use super::{tokenize, Args};

#[test]
fn tokenizes_quotes_and_whitespace() {
    assert_eq!(tokenize("  /cmd \"multi word\" x\"y z\" \"\"\tend ").ok(),
        Some(vec!["/cmd".into(), "multi word".into(), "xy z".into(), "".into(), "end".into()]));
    assert_eq!(tokenize(" \t ").ok(), Some(Vec::new()));
    assert!(tokenize("one \"unterminated").is_err());
}

#[test]
fn parses_numeric_arguments_and_rejects_invalid_values() {
    let values = ["-42".into(), "1.25".into(), "NaN".into(), "inf".into(), "no".into(), "".into()];
    let args = Args::new(&values);
    assert_eq!(args.i64(0).ok(), Some(-42));
    assert_eq!(args.f64(1).ok(), Some(1.25));
    assert!(args.f64(2).is_err());
    assert!(args.f64(3).is_err());
    assert!(args.i64(4).is_err());
    assert!(args.required(5).is_err());
    assert!(args.required(6).is_err());
}
