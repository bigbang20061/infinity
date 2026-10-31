use super::*;

#[test]
fn initial_state_is_typed_crowset_with_online_and_offline_contacts() {
    let contacts = [
        contacts::Contact { contact_id: 11, relationship: 0.0, in_watchlist: false, label_mask: 0 },
        contacts::Contact { contact_id: 12, relationship: 5.0, in_watchlist: true, label_mask: 0 },
    ];
    let Value::Reduce { data, .. } = initial_state(&contacts, &[11]) else { panic!("expected CRowset") };
    let parts = data.items().unwrap();
    assert_eq!(parts[0], Value::global("carbon.common.script.sys.crowset.CRowset"));
    let args = parts[1].items().unwrap();
    let descriptor = DbRowDescriptor::from_value(&args[0]).unwrap();
    assert_eq!(descriptor.columns, vec![("contactID".into(), DbType::I4), ("online".into(), DbType::Bool)]);
    let rows = args[1].items().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(descriptor.get(&rows[0], "contactID"), Some(Value::Int(11)));
    assert_eq!(descriptor.get(&rows[0], "online"), Some(Value::Bool(true)));
    assert_eq!(descriptor.get(&rows[1], "contactID"), Some(Value::Int(12)));
    assert_eq!(descriptor.get(&rows[1], "online"), Some(Value::Bool(false)));
}
