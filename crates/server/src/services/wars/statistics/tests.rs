use super::*;
use crate::dao::{killmails::{Killmail, wars::Projection}, wars::State};

fn war() -> War {
    War { war_id: 7, declared_by: 8, against: 9, declared_at: 10, starts_at: 11,
        state: State::Active, retracted_at: None, retracted_by: None, finishes_at: None }
}
fn row(kill_id: i64, attacker: i64, defender: i64, isk: f64) -> IndexedKillmail {
    IndexedKillmail {
        projection: Projection { kill_id, war_id: 7, attacker_side: attacker, defender_side: defender },
        killmail: Killmail { kill_id, kill_time: 12, solar_system_id: 13, victim_id: 14,
            victim_corp_id: 15, victim_alliance_id: None, victim_ship_type: 16,
            damage_taken: 17.0, final_char_id: Some(18), final_corp_id: Some(19),
            final_alliance_id: None, final_ship_type: 20, final_weapon_type: 21,
            final_damage: 22.0, final_sec_status: 0.0, isk_lost: isk, blob: String::new() },
    }
}
fn roundtrip(value: &Value) -> Value { marshal::loads(&marshal::dumps(value)).unwrap() }
fn keyval_field<'a>(value: &'a Value, key: &str) -> &'a Value {
    let Value::Instance(_, state) = value else { panic!("expected KeyVal") };
    state.get(key).unwrap()
}

#[test]
fn totals_count_killing_side_and_group_counts_victim_side() {
    let rows = vec![row(1, 8, 9, 100.0), row(2, 9, 8, 30.0), row(3, 8, 9, 5.0)];
    let base = roundtrip(&base_info(&war(), &rows));
    let values = base.items().unwrap();
    assert_eq!(values[1], Value::list([Value::tuple([Value::Int(8), Value::Int(2)]), Value::tuple([Value::Int(9), Value::Int(1)])]));
    assert_eq!(values[2], Value::list([Value::tuple([Value::Int(8), Value::Float(105.0)]), Value::tuple([Value::Int(9), Value::Float(30.0)])]));
    let groups = roundtrip(&grouped(&war(), &rows, |r| (r.projection.kill_id != 3).then_some(GROUP_FRIGATE)));
    let Value::Dict(groups) = groups else { panic!("expected group dictionary") };
    assert_eq!(groups.len(), 1);
    let group = &groups[0].1;
    assert_eq!(group.get("defenderShipLoss"), Some(&Value::Int(1)));
    assert_eq!(group.get("attackerShipLoss"), Some(&Value::Int(1)));
    assert_eq!(group.get("defenderIskLoss"), Some(&Value::Float(100.0)));
    assert_eq!(group.get("attackerIskLoss"), Some(&Value::Float(30.0)));
}

#[test]
fn class_and_owner_filters_are_strict_and_use_immutable_side() {
    let war = war();
    assert_eq!(filters(&[Value::None, Value::None], &war).unwrap(), (None, None));
    assert_eq!(filters(&[Value::Int(8), Value::Int(CLASS_CAPSULES)], &war).unwrap(), (Some(8), Some(CLASS_CAPSULES)));
    assert!(filters(&[Value::Int(10), Value::None], &war).is_err());
    assert!(filters(&[Value::Bool(true), Value::None], &war).is_err());
    assert!(filters(&[Value::None, Value::Int(10)], &war).is_err());
    assert!(filters(&[Value::None], &war).is_err());
    let row = row(1, 8, 9, 1.0);
    assert!(matches_filter(&row, Some(8), Some(CLASS_FRIGATES), Some((GROUP_FRIGATE, None))));
    assert!(!matches_filter(&row, Some(9), None, None));
    assert!(!matches_filter(&row, None, Some(CLASS_FRIGATES), None));
    assert!(matches_filter(&row, None, None, None));
    assert_eq!(ship_class(GROUP_CAPSULE, None), Some(CLASS_CAPSULES));
    assert_eq!(ship_class(GROUP_LOGISTICS_FRIGATE, None), Some(CLASS_FRIGATES));
    assert_eq!(ship_class(GROUP_COMMAND_DESTROYER, None), Some(CLASS_DESTROYERS));
    assert_eq!(ship_class(GROUP_SPECIAL_EDITION_YACHT, None), Some(CLASS_CRUISERS));
    assert_eq!(ship_class(GROUP_EXPEDITION_COMMAND_SHIP, None), Some(CLASS_BATTLECRUISERS));
    assert_eq!(ship_class(GROUP_MARAUDERS, None), Some(CLASS_BATTLESHIPS));
    assert_eq!(ship_class(GROUP_LANCER_DREADNOUGHT, None), Some(CLASS_CAPITALSHIPS));
    assert_eq!(ship_class(GROUP_TRANSPORT_SHIP, None), Some(CLASS_INDUSTRIALS));
    assert_eq!(ship_class(-1, Some(CATEGORY_STARBASE)), Some(CLASS_POS));
    assert_eq!(ship_class(-1, Some(CATEGORY_STRUCTURE)), Some(CLASS_STRUCTURES));
    assert_eq!(ship_class(-1, None), None);
}

#[test]
fn report_roundtrip_keeps_unknown_alliances_and_requested_war() {
    let report = roundtrip(&killmail_value(row(1, 8, 9, 1.0).killmail, Some(7)));
    assert_eq!(keyval_field(&report, "victimAllianceID"), &Value::None);
    assert_eq!(keyval_field(&report, "finalAllianceID"), &Value::None);
    assert_eq!(keyval_field(&report, "warID"), &Value::Int(7));
    assert_eq!(keyval_field(&report, "finalCorporationID"), &Value::Int(19));
}

#[test]
fn bound_identity_is_not_taken_from_method_arguments() {
    let bind = Value::tuple([Value::Int(7)]);
    let args = [Value::Int(99)];
    let kwargs = Value::None;
    let call = Call { service: "warStatisticMgr", method: "GetBaseInfo", args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&bind) };
    assert_eq!(bound_id(&call).unwrap(), 7);
    let bool_bind = Value::Bool(true);
    assert!(bound_id(&Call { bind_params: Some(&bool_bind), ..call }).is_err());
}
