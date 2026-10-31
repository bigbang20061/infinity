use super::parse;

fn args(words: &[&str]) -> Vec<String> { words.iter().map(|s| (*s).to_owned()).collect() }

#[test]
fn parses_current_system_and_random_count() {
    assert!(matches!(parse(&args(&["random", "3", "here"])), Ok(("random", 3))));
    assert!(matches!(parse(&args(&["random", "here"])), Ok(("random", 1))));
    assert!(matches!(parse(&args(&["status"])), Ok(("status", 1))));
    assert!(parse(&args(&["random", "0"])).is_err());
    assert!(parse(&args(&["clear", "all"])).is_err());
    assert!(parse(&args(&["status", "here", "extra"])).is_err());
}
