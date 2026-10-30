use super::*;

#[test]
fn station_preserves_location_and_defaults_null_owner() {
    let mut row = crate::sde::map::Station {
        station_id: 10, type_id: None, owner_id: None, solar_system_id: Some(20),
        constellation_id: Some(30), region_id: Some(40), operation_id: None,
        x: None, y: None, z: None, security: None,
    };
    let at = market_station(&row);
    assert_eq!((at.id, at.system, at.constellation, at.region, at.owner), (10, 20, 30, 40, 0));
    row.owner_id = Some(50);
    assert_eq!(market_station(&row).owner, 50);
}
