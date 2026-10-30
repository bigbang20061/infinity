use super::*;

#[test]
fn race_specific_falls_back_to_zero_only_within_group() {
    let wrecks = Wrecks { mappings: HashMap::from([(10, HashMap::from([(0, 100), (2, 102)])), (20, HashMap::from([(3, 203)]))]) };
    assert_eq!(wrecks.get(10, 2), Some(102));
    assert_eq!(wrecks.get(10, 7), Some(100));
    assert_eq!(wrecks.get(20, 7), None);
    assert_eq!(wrecks.get(99, 0), None);
}
