//! Access-group RPC authorization and durable audit regressions.
mod permissions;

use crate::{client::Client, db::Db, services::{Call, CallError}};
use marshal::Value;

async fn rpc(c: &mut Client, method: &str, args: &[Value]) -> Result<Value, CallError> {
    let kwargs = Value::Dict(vec![]);
    crate::services::dispatch(c, &Call { service: "ownerGroupManager", method, args, kwargs: &kwargs, bound: false, bind_params: None }).await
}

fn ids(ids: &[i64]) -> Value { Value::list(ids.iter().copied().map(Value::Int)) }
fn grants(rows: &[(i64, i32)]) -> Value {
    Value::Dict(rows.iter().map(|&(id, kind)| (Value::Int(id), Value::Int(kind.into()))).collect())
}

async fn cleanup(db: &Db, characters: &[i64]) {
    db.execute("DELETE FROM game.access_group_logs WHERE group_id IN (SELECT group_id FROM game.access_groups WHERE creator_id = ANY($1))", &[&characters]).await.unwrap();
    db.execute("DELETE FROM game.access_group_logs WHERE character_id = ANY($1)", &[&characters]).await.unwrap();
    db.execute("DELETE FROM game.access_groups WHERE creator_id = ANY($1)", &[&characters]).await.unwrap();
}
