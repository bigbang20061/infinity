//! Saved personal fittings, mirroring charFittingMgr and fittingSvc.py.
//! Uses game.fittings; fittings owned by other characters are unsupported.
use crate::services::{db_err, keyval, value_field, value_id_list, Call, CallError, CallResult};
use crate::dao::items::fitting as fitting_dao;
use crate::client::Client;
use marshal::Value;

fn fitting_value(r: &fitting_dao::Fitting) -> (Value, Value) {
    let id = r.fitting_id;
    let (types, flags, qty) = (&r.fit_types, &r.fit_flags, &r.fit_qty);
    let fit_data = types.iter().zip(flags).zip(qty).map(|((t, f), q)| Value::tuple([Value::Int(*t as i64), Value::Int(*f as i64), Value::Int(*q as i64)]));
    let fitting = keyval([
        ("fittingID", Value::Int(id)),
        ("ownerID", Value::Int(r.owner_id)),
        ("name", Value::unicode(r.name.clone())),
        ("description", Value::unicode(r.description.clone())),
        ("shipTypeID", Value::Int(r.ship_type_id as i64)),
        ("fitData", Value::list(fit_data)),
    ]);
    (Value::Int(id), fitting)
}

async fn fittings(c: &mut Client, call: &Call<'_>) -> CallResult {
    let char_id = c.session.int("charid").unwrap_or(0);
    let owner = call.args.first().and_then(|v| v.as_int()).unwrap_or(char_id);
    if owner != char_id {
        return Ok(match call.method {
            "GetFittings" => Value::Dict(vec![]),
            _ => Value::None,
        });
    }
    let db = &c.db;
    Ok(match call.method {
        // {fittingID: KeyVal(fittingID, ownerID, name, description, shipTypeID, fitData)}
        "GetFittings" => {
            let rows = fitting_dao::fittings(db, owner).await.map_err(db_err)?;
            Value::Dict(rows.iter().map(fitting_value).collect())
        }
        // SaveManyFittings(ownerID, {tempID: fitting}) -> [KeyVal(tempFittingID, realFittingID)]
        "SaveManyFittings" => {
            let mut saved = vec![];
            if let Some(Value::Dict(fits)) = call.args.get(1) {
                for (temp, fit) in fits {
                    let text = |k: &str| value_field(fit, k).and_then(|v| v.as_text()).map(|t| t.into_owned()).unwrap_or_default();
                    let ship_type = value_field(fit, "shipTypeID").and_then(|v| v.as_int()).unwrap_or(0) as i32;
                    let (mut types, mut flags, mut qty) = (vec![], vec![], vec![]);
                    for row in value_field(fit, "fitData").and_then(|v| v.items()).unwrap_or_default() {
                        if let Some([t, f, q]) = row.items() {
                            types.push(t.as_int().unwrap_or(0) as i32);
                            flags.push(f.as_int().unwrap_or(0) as i32);
                            qty.push(q.as_int().unwrap_or(1) as i32);
                        }
                    }
                    let id = fitting_dao::save(db, owner, &text("name"), &text("description"), ship_type, &types, &flags, &qty).await.map_err(db_err)?;
                    saved.push(keyval([("tempFittingID", temp.clone()), ("realFittingID", Value::Int(id))]));
                }
            }
            Value::List(saved)
        }
        "DeleteFitting" => {
            let id = call.args.get(1).and_then(|v| v.as_int()).unwrap_or(0);
            fitting_dao::delete(db, owner, id).await.map_err(db_err)?;
            Value::None
        }
        "DeleteManyFittings" => {
            let ids = value_id_list(call.args.get(1));
            fitting_dao::delete_many(db, owner, &ids).await.map_err(db_err)?;
            Value::None
        }
        _ => return Err(CallError::Internal(anyhow::anyhow!("charFittingMgr::{} not handled", call.method))),
    })
}

/// UpdateNameAndDescription(fittingID, ownerID, name, description)
async fn rename_fitting(c: &Client, call: &Call<'_>) -> CallResult {
    let char_id = c.session.int("charid").unwrap_or(0);
    let id = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
    let text = |i: usize| call.args.get(i).and_then(|v| v.as_text()).map(|t| t.into_owned()).unwrap_or_default();
    fitting_dao::rename(&c.db, id, char_id, &text(2), &text(3)).await.map_err(db_err)?;
    Ok(Value::None)
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    Some(match call.method { "UpdateNameAndDescription" => rename_fitting(c, call).await, _ => fittings(c, call).await })
}
