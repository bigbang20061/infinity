use super::*;

#[test]
fn unix_milliseconds_convert_to_filetime() {
    assert_eq!(filetime_from_unix_ms(0), FILETIME_UNIX_EPOCH);
    assert_eq!(filetime_from_unix_ms(1), FILETIME_UNIX_EPOCH + 10_000);
    assert_eq!(filetime_from_unix_ms(-1), FILETIME_UNIX_EPOCH - 10_000);
}

#[test]
fn signed_filetime_seconds_conversions() {
    assert_eq!(unix_seconds_from_filetime(FILETIME_UNIX_EPOCH + FILETIME_SECOND + 1), 1);
    assert_eq!(unix_seconds_from_filetime(FILETIME_UNIX_EPOCH - FILETIME_SECOND - 1), -1);
    assert_eq!(unix_seconds_from_filetime(FILETIME_UNIX_EPOCH - 1), 0);
    assert_eq!(filetime_ticks_from_seconds(2), 2 * FILETIME_SECOND);
    assert_eq!(filetime_ticks_from_seconds(-2), -2 * FILETIME_SECOND);
    assert_eq!(filetime_ticks_from_seconds(0), 0);
}

#[test]
fn filetime_timestamp_milliseconds_round_up() {
    assert_eq!(unix_ms_from_filetime_ceil(FILETIME_UNIX_EPOCH), 0);
    assert_eq!(unix_ms_from_filetime_ceil(FILETIME_UNIX_EPOCH + FILETIME_TICKS_PER_MS), 1);
    assert_eq!(unix_ms_from_filetime_ceil(FILETIME_UNIX_EPOCH + FILETIME_TICKS_PER_MS + 1), 2);
    assert_eq!(unix_ms_from_filetime_ceil(FILETIME_UNIX_EPOCH - FILETIME_TICKS_PER_MS), -1);
    assert_eq!(unix_ms_from_filetime_ceil(FILETIME_UNIX_EPOCH - FILETIME_TICKS_PER_MS + 1), 0);
}
