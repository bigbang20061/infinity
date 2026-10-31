use super::tokenize;

#[test]
fn tokenizes_quotes_and_whitespace() {
    assert_eq!(tokenize("  /cmd \"multi word\" x\"y z\" \"\"\tend ").ok(),
        Some(vec!["/cmd".into(), "multi word".into(), "xy z".into(), "".into(), "end".into()]));
    assert_eq!(tokenize(" \t ").ok(), Some(Vec::new()));
    assert!(tokenize("one \"unterminated").is_err());
}
