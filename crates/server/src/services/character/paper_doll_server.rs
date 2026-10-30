//! Stored appearance and portraits for `paperDollServer`, used by the client
//! character creator. Reads and writes `game.characters` through the DAO.
//! No server-side paperdoll rendering or appearance validation is performed.
use super::*;

/// A character's stored paperdoll blob (`appearance` or `portrait`), as the client sent it.
async fn doll_blob(c: &Client, id: i64, column: crate::dao::characters::social::DollColumn) -> Result<Value, CallError> {
    let row = crate::dao::characters::social::doll_blob(&c.db, id, column).await.map_err(db_err)?;
    match row {
        Some(blob) => marshal::loads(&blob).map_err(|e| CallError::Internal(anyhow::anyhow!("{} of {id}: {e:?}", column.name()))),
        None => Ok(Value::None),
    }
}

/// The character creator leaves unset sculpt weights and resource variations as None;
/// the client reads them like database columns (`None < 0.0` then `abs(None)` in
/// character.ConvertWeightKeyValue), so serve them as zeros.
fn zero_doll_nulls(v: &mut Value) {
    match v {
        Value::Instance(_, state) => zero_doll_nulls(state),
        Value::Dict(pairs) => pairs.iter_mut().for_each(|(_, v)| zero_doll_nulls(v)),
        Value::List(items) => items.iter_mut().for_each(zero_doll_nulls),
        Value::NewObj { data, .. } => {
            if let Value::Tuple(outer) = data.as_mut() {
                if let Some(Value::Tuple(args)) = outer.first_mut() {
                    let float = matches!(args.first(), Some(Value::Global(c)) if c.ends_with("SculptingRow"));
                    for a in args.iter_mut().skip(1).filter(|a| a.is_none()) {
                        *a = if float { Value::Float(0.0) } else { Value::Int(0) };
                    }
                }
            }
        }
        _ => {}
    }
}

/// paperDollServer.UpdateExistingCharacter{Full,Limited}(charID, dollInfo, portraitInfo, dollExists)
async fn store_doll(c: &Client, call: &Call<'_>) -> CallResult {
    let id = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
    let appearance = marshal::dumps(call.args.get(1).unwrap_or(&Value::None));
    let portrait = marshal::dumps(call.args.get(2).unwrap_or(&Value::None));
    crate::dao::characters::social::store_doll(&c.db, id, c.login.user_id, &appearance, &portrait).await.map_err(db_err)?;
    Ok(Value::None)
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let me = char_id(c);
    Some(match (call.service, call.method) {
        ("paperDollServer", "GetMyPaperDollData") | ("paperDollServer", "GetPaperDollData") => {
            let id = call.args.first().and_then(|v| v.as_int()).unwrap_or(me);
            doll_blob(c, id, crate::dao::characters::social::DollColumn::Appearance).await.map(|mut v| {
                zero_doll_nulls(&mut v);
                v
            })
        }
        // (portraits, portraitData): ccSvc reads portraits[0].
        ("paperDollServer", "GetPaperDollPortraitDataFor") => {
            let id = call.args.first().and_then(|v| v.as_int()).unwrap_or(me);
            doll_blob(c, id, crate::dao::characters::social::DollColumn::Portrait).await.map(|p| {
                let portraits = if p.is_none() { vec![] } else { vec![p] };
                Value::tuple([Value::List(portraits), Value::None])
            })
        }
        ("paperDollServer", "UpdateExistingCharacterFull") | ("paperDollServer", "UpdateExistingCharacterLimited") => {
            store_doll(c, call).await
        }
        _ => return None,
    })
}
