//! Repair quotes for `repairSvc`, used by the client's repair interface.
//! Reads `game.items` through the item DAO and raw type attributes from startup SDE.
//! Docking already repairs ships; quoted damage and repair costs are zero.
use super::*;

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let me = char_id(c);
    Some(match (call.service, call.method) {
        ("repairSvc", "GetRepairQuotes") => {
            let ids = value_id_list(call.args.first());
            match crate::dao::items::social::repair_quotes(&c.db, &ids, me).await {
                Ok(rows) => Ok(Value::Dict(
                    rows.iter()
                        .map(|r| {
                            let id = r.item_id;
                            let quote = keyval([
                                ("itemID", Value::Int(id)),
                                ("typeID", Value::Int(r.type_id)),
                                ("damage", Value::Float(0.0)),
                                ("maxHealth", Value::Float(c.sde.types.attribute(r.type_id as i32, ATTR_STRUCTURE_HP).unwrap_or(0.0))),
                                ("costToRepairOneUnitOfDamage", Value::Float(0.0)),
                            ]);
                            (Value::Int(id), Value::list([quote]))
                        })
                        .collect(),
                )),
                Err(e) => Err(db_err(e)),
            }
        }
        _ => return None,
    })
}
