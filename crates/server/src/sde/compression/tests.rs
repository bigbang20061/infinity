use super::*;

fn row(source: Option<&str>, output: Option<&str>) -> dao::CompressionRow {
    dao::CompressionRow { source: source.map(str::to_owned), output: output.map(str::to_owned) }
}

#[test]
fn validates_mappings_before_exposing_them() {
    let exists = |id| [1, 2, 3].contains(&id);
    let mappings = Compression::from_rows(vec![row(Some("1"), Some("2")), row(Some("1"), Some("2"))], exists).unwrap();
    assert_eq!(mappings.get(1), Some(2));
    assert_eq!(mappings.get(3), None);
    assert!(Compression::from_rows(vec![], exists).is_err());
    for rows in [
        vec![row(None, Some("2"))],
        vec![row(Some("1"), None)],
        vec![row(Some("bad"), Some("2"))],
        vec![row(Some("1"), Some("bad"))],
        vec![row(Some("0"), Some("2"))],
        vec![row(Some("1"), Some("-2"))],
        vec![row(Some("1"), Some("1"))],
        vec![row(Some("4"), Some("2"))],
        vec![row(Some("1"), Some("4"))],
        vec![row(Some("1"), Some("2")), row(Some("1"), Some("3"))],
    ] {
        assert!(Compression::from_rows(rows, exists).is_err());
    }
}
