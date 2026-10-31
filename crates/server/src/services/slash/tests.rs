use super::*;

#[test]
fn isk_grammar_and_argument_count() {
    let parse = |args: &[&str]| parse_amount(&args.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(), ISK_INTEGER_DIGITS, ISK_DECIMALS);
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
    let parse = |args: &[&str]| parse_amount(&args.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(), PLEX_DIGITS, 0);
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
    assert_eq!(messages::gm::gm_balance("1.23", None), "balance 1.23 ISK");
    assert_eq!(messages::gm::gm_balance("999.01", Some(999.0)), "balance 999.01 ISK (client shows 999)");
    assert_eq!(messages::gm::gm_plex_balance("12", None), "vault 12 PLEX");
    assert_eq!(messages::gm::gm_plex_balance("1000", Some(999)), "vault 1000 PLEX (client shows 999)");
}

#[test]
fn dogma_grammar() {
    use dogma::{Action, Target};
    for (args, expected) in [
        (vec!["ship", "list"], (Target::Ship, Action::List)),
        (vec!["flag:27", "damageMultiplier", "3"], (Target::Flag(27), Action::Set("damageMultiplier", 3.0))),
        (vec!["123", "37"], (Target::Item(123), Action::Query("37"))),
        (vec!["ship", "37", "reset"], (Target::Ship, Action::Reset("37"))),
    ] {
        let owned: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
        assert_eq!(dogma::parse(&owned), Some(expected));
    }
    for args in [&[][..], &["ship"][..], &["flag:x", "37"][..], &["flag:-1", "37"][..],
        &["0", "37"][..], &["ship", "", "1"][..], &["ship", "37", "NaN"][..],
        &["ship", "37", "inf"][..], &["ship", "37", "-inf"][..],
        &["ship", "37", "x"][..], &["ship", "list", "1"][..], &["ship", "37", "1", "2"][..]] {
        let owned: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
        assert_eq!(dogma::parse(&owned), None, "{args:?}");
    }
}

#[test]
fn dogma_runtime_and_character_attributes_are_denied_for_all_actions() {
    use dogma::{denied, Action};
    for args in [&["ship", "skillPoints"][..], &["ship", "skillPoints", "1"][..], &["ship", "skillPoints", "reset"][..]] {
        let owned: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
        let (_, action) = dogma::parse(&owned).unwrap();
        assert_eq!(match action { Action::Query(a) | Action::Set(a, _) | Action::Reset(a) => a, Action::List => panic!("expected attribute action") }, "skillPoints");
        assert_eq!(denied(9999, |name, id| name == "skillPoints" && id == 9999), Some(messages::gm::GM_DOGMA_RUNTIME));
        assert_eq!(denied(164, |_, _| false), Some(messages::gm::GM_DOGMA_CHARACTER));
        assert_eq!(denied(9999, |_, _| false), None);
    }
}
