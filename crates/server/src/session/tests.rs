use super::*;

#[test]
fn attributes_are_read_only_and_sorted() {
    let mut session = Session::new(7);
    session.apply(vec![("z", Value::Int(2)), ("a", Value::Int(1))]);
    let rows: Vec<_> = session.attributes().collect();
    assert_eq!(rows, vec![("a", &Value::Int(1)), ("z", &Value::Int(2))]);
    assert_eq!(session.int("a"), Some(1));
}
