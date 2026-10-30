use super::*;

#[test]
fn facility_services_keep_activity_order() {
    let mut facility = Facility { id: 1, type_id: 1, owner: 1, system: 1, security: 0.0, factory: false, laboratory: false };
    assert!(facility.activities().is_empty());
    facility.factory = true;
    assert_eq!(facility.activities(), vec![MANUFACTURING]);
    facility.factory = false;
    facility.laboratory = true;
    assert_eq!(facility.activities(), vec![RESEARCH_TIME, RESEARCH_MATERIAL, COPYING, INVENTION]);
    facility.factory = true;
    assert_eq!(facility.activities(), vec![MANUFACTURING, RESEARCH_TIME, RESEARCH_MATERIAL, COPYING, INVENTION]);
}

fn station_without_services(type_id: Option<i64>, solar_system_id: Option<i64>) -> crate::sde::map::Station {
    crate::sde::map::Station {
        station_id: 1, type_id, owner_id: None, solar_system_id, constellation_id: None,
        region_id: None, operation_id: None, x: None, y: None, z: None, security: None,
    }
}

#[test]
#[should_panic(expected = "SDE industry station has no type_id")]
fn station_without_services_requires_type_before_filtering() {
    Facility::from_station_services(&station_without_services(None, None), false, false);
}

#[test]
#[should_panic(expected = "SDE industry station has no solar_system_id")]
fn station_without_services_requires_system_before_filtering() {
    Facility::from_station_services(&station_without_services(Some(1), None), false, false);
}
