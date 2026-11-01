use super::*;

fn fixture() -> War {
    War { war_id: 7, declared_by: 8, against: 9, declared_at: 10, starts_at: 11,
        state: State::Pending, retracted_at: None, retracted_by: None, finishes_at: None }
}

fn roundtrip(value: &Value) -> Value { marshal::loads(&marshal::dumps(value)).unwrap() }

fn fields(value: &Value) -> &[Value] {
    let Value::Reduce { data, .. } = value else { panic!("expected War reduce") };
    let [Value::Global(class), args] = data.items().unwrap() else { panic!("expected callable and arguments") };
    assert_eq!(class, "warUtil.War");
    args.items().unwrap()
}

#[test]
fn war_roundtrip_preserves_nulls_and_retraction() {
    let mut row = fixture();
    let value = roundtrip(&war(&row));
    let f = fields(&value);
    assert_eq!(f.len(), 17);
    assert_eq!(&f[..8], &[Value::Int(7), Value::Int(8), Value::Int(9), Value::Int(10), Value::None, Value::None, Value::Int(11), Value::None]);
    assert_eq!(f[8], Value::None);
    assert_eq!(f[9], Value::Bool(true));
    assert_eq!(f[10], Value::Dict(vec![]));
    assert_eq!(f[11], Value::None);
    assert_eq!(f[12], Value::Bool(false));
    assert_eq!(f[13], Value::Bool(true));
    assert_eq!(f[14], Value::None);
    assert_eq!(f[15], Value::None);
    assert_eq!(f[16], Value::Int(STARTED_PLAYER_DECLARED));
    row.retracted_at = Some(12);
    row.retracted_by = Some(8);
    row.finishes_at = Some(13);
    row.state = State::Cooldown;
    let value = roundtrip(&war(&row));
    let f = fields(&value);
    assert_eq!((f[4].clone(), f[5].clone(), f[7].clone()), (Value::Int(13), Value::Int(12), Value::Int(8)));
    assert_eq!(f[13], Value::Bool(false));
    assert_eq!(f[14], Value::Int(ENDED_MUTUAL_END));
    row.state = State::Ended;
    assert_eq!(fields(&war(&row))[13], Value::Bool(false));
}

#[test]
fn history_list_and_registry_dictionary_roundtrip() {
    let row = fixture();
    let list = roundtrip(&war_list(&[row.clone()]));
    let [entry] = list.items().unwrap() else { panic!("expected one war") };
    assert_eq!(fields(entry)[0], Value::Int(row.war_id));
    let dict = roundtrip(&wars(&[row]));
    let Value::Dict(entries) = dict else { panic!("expected war dictionary") };
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0, Value::Int(7));
    assert_eq!(fields(&entries[0].1)[0], Value::Int(7));
}

#[test]
fn changes_compare_encoded_fields_and_not_internal_state() {
    let old = fixture();
    let mut new = old.clone();
    new.state = State::Active;
    assert_eq!(changes(Some(&old), &new), Value::Dict(vec![]));
    new.state = State::Cooldown;
    new.retracted_at = Some(12);
    new.retracted_by = Some(8);
    new.finishes_at = Some(13);
    assert_eq!(roundtrip(&changes(Some(&old), &new)), Value::Dict(vec![
        (Value::str("timeFinished"), Value::tuple([Value::None, Value::Int(13)])),
        (Value::str("retracted"), Value::tuple([Value::None, Value::Int(12)])),
        (Value::str("retractedBy"), Value::tuple([Value::None, Value::Int(8)])),
        (Value::str("canBeRetracted"), Value::tuple([Value::Bool(true), Value::Bool(false)])),
        (Value::str("reasonEnded"), Value::tuple([Value::None, Value::Int(ENDED_MUTUAL_END)])),
    ]));
    let Value::Dict(created) = changes(None, &old) else { panic!("expected changes dictionary") };
    assert_eq!(created.len(), 17);
    assert_eq!(created[0], (Value::str("warID"), Value::tuple([Value::None, Value::Int(7)])));
}
