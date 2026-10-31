use super::*;
use crate::dao::items::creation;

#[path = "../combat/groups_health/tests/fixture.rs"]
mod fixture;
mod capacitor_priming_db;

#[test]
fn item_attribute_union_overlays_and_adds() {
    let mut attrs = BTreeMap::from([(1, 10.0), (2, 20.0)]);
    overlay_overrides(&mut attrs, [(2, 30.0), (3, 40.0)]);
    assert_eq!(attrs, BTreeMap::from([(1, 10.0), (2, 30.0), (3, 40.0)]));
}
