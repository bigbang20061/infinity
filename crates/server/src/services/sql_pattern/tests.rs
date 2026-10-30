use super::matches_pattern;

#[test]
fn sql_like_patterns_match_unicode_wildcards_and_escapes() {
    let cases = [
        ("", "", true),
        ("", "%", true),
        ("", "_", false),
        ("a", "", false),
        ("星船", "__", true),
        ("星船", "_", false),
        ("巡洋舰", "%洋%", true),
        ("abc", "a_c", true),
        ("ac", "a_c", false),
        ("a%b", r"a\%b", true),
        ("axxb", r"a\%b", false),
        ("a_b", r"a\_b", true),
        ("axb", r"a\_b", false),
        (r"a\b", r"a\\b", true),
        ("ab", r"a\\b", false),
        ("ababc", "%ab%c", true),
        ("ababa", "%ab%c", false),
        ("星船星舰", "%星舰", true),
        ("星船星舰", "%星船舰", false),
        ("星船%舰", r"%船\%_", true),
        ("a%b%", r"%\%b%", true),
        ("abc%", r"%c\%", true),
        ("abc", r"%c\%", false),
        ("mordu’s special warfare unit operative", "%mordu’s%", true),
        ("♦ venture", "_ venture", true),
        ("salvation angel´s shipment", "%angel´s%", true),
        ("greater good – distasteful matters (2 of 5)", "%good _ distasteful%", true),
        ("the sarum prime system – planet iii: “mekhios” graveyard", "%“mekhios”%", true),
        ("istanbul", "i%", true),
        ("i\u{307}stanbul", "i_stanbul", true),
        (r"a\", r"a\", false),
    ];
    for (name, pattern, expected) in cases {
        assert_eq!(matches_pattern(name, pattern), expected, "name={name:?}, pattern={pattern:?}");
    }
}

#[test]
fn literal_contains_and_general_patterns_preserve_like_semantics() {
    let cases = [
        ("巡洋舰", "%洋%", true),
        ("巡洋舰", "%星%", false),
        ("", "%%", true),
        ("星船", "%%", true),
        ("星船", "%星_％%", false),
        ("星船舰", "%星_舰%", true),
        ("星甲舰", "%星%舰%", true),
        ("星%舰", r"%星\%舰%", true),
        ("星甲舰", r"%星\%舰%", false),
        (r"星\舰", r"%星\\舰%", true),
        ("星舰", r"%星\\舰%", false),
    ];
    for (name, pattern, expected) in cases {
        assert_eq!(matches_pattern(name, pattern), expected, "name={name:?}, pattern={pattern:?}");
    }
}
