use super::*;

#[test]
fn regrowth_deadline_rounds_up_to_milliseconds() {
    let whole = crate::client::filetime_from_unix_ms(42);
    assert_eq!(unix_ms_from_filetime_ceil(whole), 42);
    assert_eq!(unix_ms_from_filetime_ceil(whole + 1), 43);
}

#[test]
fn ore_bands_follow_security() {
    assert!(field_groups(Field::Ore, 0.9, false).0.contains(&"Veldspar"));
    assert!(field_groups(Field::Ore, 0.3, false).0.contains(&"Jaspet"));
    assert!(field_groups(Field::Ore, -0.4, false).0.contains(&"Arkonor"));
    assert!(field_groups(Field::Gas, -1.0, true).0.iter().all(|g| g.starts_with("Fullerite")));
    assert!(rock_radius(Field::Ore, 25_000.0) < 10_000.0);
}

#[test]
fn output_reuses_fully_consumed_source_quantity() {
    assert!(matches!(output_stack_quantity(None, 12), Ok(12)));
    assert!(matches!(output_stack_quantity(Some(3), 12), Ok(15)));
}

#[test]
fn output_overflow_is_a_typed_refusal() {
    for result in [
        add_output(i64::MAX, 1),
        output_stack_quantity(None, i64::from(i32::MAX) + 1).map(i64::from),
        output_stack_quantity(Some(i32::MAX), 1).map(i64::from),
    ] {
        assert!(matches!(result, Err(CallError::User { msg, .. }) if msg == "CustomNotify"));
    }
}
