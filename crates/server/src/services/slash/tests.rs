use super::*;

#[test]
fn isk_grammar_and_argument_count() {
    let parse = |args: &[&str]| parse_amount(args, ISK_INTEGER_DIGITS, ISK_DECIMALS);
    for (input, expected) in [("0", "0"), ("1.2", "1.2"), (",1,,2,.3,4,", "12.34")] {
        assert_eq!(parse(&[input]).as_deref(), Some(expected));
    }
    for input in ["", ",,,", ".1", "1.", "1.234", "1.2.3", "-1", "+1", "1e2", "１２", "1a"] {
        assert_eq!(parse(&[input]), None, "{input}");
    }
    assert!(parse(&[]).is_none());
    assert!(parse(&["1", "2"]).is_none());
    assert!(parse(&[&"9".repeat(34)]).is_some());
    assert!(parse(&[&"9".repeat(35)]).is_none());
    assert!(parse(&["1.23"]).is_some());
    assert!(parse(&["1.234"]).is_none());
}

#[test]
fn plex_grammar_and_argument_count() {
    let parse = |args: &[&str]| parse_amount(args, PLEX_DIGITS, 0);
    assert_eq!(parse(&[",1,,2,"]).as_deref(), Some("12"));
    assert!(parse(&[&"9".repeat(36)]).is_some());
    assert!(parse(&[&"9".repeat(37)]).is_none());
    for input in ["", ",", "1.0", "-1", "+1", "1e2", "１２"] {
        assert_eq!(parse(&[input]), None, "{input}");
    }
    assert!(parse(&[]).is_none());
    assert!(parse(&["1", "2"]).is_none());
}

#[test]
fn exact_balance_clamp_and_echo() {
    let isk_cap = format!("{CLIENT_MAX_ISK:.0}");
    assert!(!exceeds_cap(&format!("000{isk_cap}.00"), &isk_cap));
    assert!(exceeds_cap(&format!("{isk_cap}.01"), &isk_cap));
    assert!(!exceeds_cap("0000.00", &isk_cap));
    assert!(exceeds_cap("1000", "999"));
    assert!(!exceeds_cap("998.99", "999"));
    assert!(exceeds_cap("999.01", "999"));
    let plex_cap = CLIENT_MAX_PLEX.to_string();
    assert!(!exceeds_cap(&format!("000{plex_cap}"), &plex_cap));
    assert!(exceeds_cap(&format!("{plex_cap}0"), &plex_cap));
    assert_eq!(messages::gm_balance("1.23", None), "balance 1.23 ISK");
    assert_eq!(messages::gm_balance("999.01", Some(999.0)), "balance 999.01 ISK (client shows 999)");
    assert_eq!(messages::gm_plex_balance("12", None), "vault 12 PLEX");
    assert_eq!(messages::gm_plex_balance("1000", Some(999)), "vault 1000 PLEX (client shows 999)");
}
