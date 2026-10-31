//! Script access to sampled world state and fixed Rust actor operations.
//! Notifications use the pilot registry and space mutations use the system actor.
//! Missing systems are never loaded by these operations; Lua never runs in an actor.

use std::sync::Arc;
use mlua::{Lua, Result, Table, Value};
use crate::{consts::CATEGORY_ENTITY, lua::value::sequence, space::World};
use super::Resources;

fn positive(id: i64) -> Result<i64> {
    if id <= 0 { return Err(mlua::Error::runtime("ID must be positive")); }
    Ok(id)
}

fn keys(table: &Table, allowed: &[&str]) -> Result<()> {
    for entry in table.clone().pairs::<Value, Value>() {
        let (key, _) = entry?;
        let Value::String(key) = key else { return Err(mlua::Error::runtime("unexpected operation argument")); };
        if !allowed.iter().any(|name| key.as_bytes().as_ref() == name.as_bytes()) {
            return Err(mlua::Error::runtime("unexpected operation argument"));
        }
    }
    Ok(())
}

enum Operation {
    Notify { character: i64, event: String, args: Vec<marshal::Value> },
    Remove { id: i64 },
    Spawn { type_id: i32, corp: i64, position: [f64; 3], target: Option<i64> },
}

fn operation(op: &str, args: Table) -> Result<Operation> {
    match op {
        "notify" => {
            keys(&args, &["character", "event", "args"])?;
            let character = positive(args.raw_get("character")?)?;
            let event: String = args.raw_get("event")?;
            if event.is_empty() { return Err(mlua::Error::runtime("event must not be empty")); }
            Ok(Operation::Notify { character, event, args: sequence(args.raw_get("args")?)? })
        }
        "remove_ball" => {
            keys(&args, &["id"])?;
            Ok(Operation::Remove { id: positive(args.raw_get("id")?)? })
        }
        "spawn_npc" => {
            keys(&args, &["type_id", "corp", "position", "aggro_target"])?;
            let type_id: i32 = args.raw_get("type_id")?;
            positive(i64::from(type_id))?;
            let corp = positive(args.raw_get("corp")?)?;
            let target: Option<i64> = args.raw_get("aggro_target")?;
            if let Some(id) = target { positive(id)?; }
            let position: Table = args.raw_get("position")?;
            let mut count = 0;
            for entry in position.clone().pairs::<Value, Value>() {
                let (key, _) = entry?;
                if !matches!(key, Value::Integer(1..=3)) { return Err(mlua::Error::runtime("position must have exactly three coordinates")); }
                count += 1;
            }
            if count != 3 { return Err(mlua::Error::runtime("position must have exactly three coordinates")); }
            let position = [position.raw_get::<f64>(1)?, position.raw_get::<f64>(2)?, position.raw_get::<f64>(3)?];
            if !position.iter().all(|v| v.is_finite()) { return Err(mlua::Error::runtime("position must be finite")); }
            Ok(Operation::Spawn { type_id, corp, position, target })
        }
        _ => Err(mlua::Error::runtime("unknown world operation")),
    }
}

pub(super) fn register(lua: &Lua, env: &Table, resources: &Resources) -> Result<()> {
    let table = lua.create_table()?;
    let handle = Arc::clone(&resources.world);
    table.set("notify_pilot", lua.create_function(move |_, (character, event, args): (i64, String, Table)| {
        positive(character)?;
        if event.is_empty() { return Err(mlua::Error::runtime("event must not be empty")); }
        handle.notify_pilot(character, &event, sequence(args)?);
        Ok(())
    })?)?;
    let handle = Arc::clone(&resources.world);
    table.set("ball", lua.create_async_function(move |lua, (system, id): (i64, i64)| {
        let handle = Arc::clone(&handle);
        async move {
            positive(system)?;
            positive(id)?;
            handle.with(system, move |s| s.sampled_ball(id)).await.flatten().map(|ball| {
                let row = lua.create_table()?;
                row.set("id", ball.id)?;
                row.set("type_id", ball.type_id)?;
                row.set("owner_id", ball.owner_id)?;
                row.set("position", lua.create_sequence_from(ball.pos)?)?;
                row.set("velocity", lua.create_sequence_from(ball.vel)?)?;
                row.set("radius", ball.radius)?;
                Ok(row)
            }).transpose()
        }
    })?)?;
    let handle: Arc<World> = Arc::clone(&resources.world);
    let db = resources.db.clone();
    let sde = Arc::clone(&resources.sde);
    table.set("schedule", lua.create_async_function(move |_, (system, delay, op, args): (i64, i64, String, Table)| {
        let handle = Arc::clone(&handle);
        let db = db.clone();
        let sde = Arc::clone(&sde);
        async move {
            positive(system)?;
            let delay = u64::try_from(delay).map_err(|_| mlua::Error::runtime("delay must be nonnegative"))?;
            let operation = operation(&op, args)?;
            if let Operation::Spawn { type_id, .. } = &operation {
                let valid = sde.types.get(*type_id).and_then(|data| data.group_id)
                    .and_then(|id| sde.types.group(id)).is_some_and(|group| group.category_id == Some(CATEGORY_ENTITY));
                if !valid { return Err(mlua::Error::runtime("type must be an SDE entity")); }
            }
            let world = Arc::clone(&handle);
            Ok(handle.with(system, move |s| {
                s.schedule(delay, move |s| match operation {
                    Operation::Notify { character, event, args } => s.notify_pilot(character, &event, args),
                    Operation::Remove { id } => { s.remove_ball(id); }
                    Operation::Spawn { type_id, corp, position, target } => {
                        s.defer(async move {
                            let data = sde.types.get(type_id).expect("validated immutable SDE type");
                            if let Err(error) = crate::services::combat::npc::spawn(&db, &world, system, data, corp, position, crate::space::now_ms(), target).await {
                                match error {
                                    crate::services::CallError::Internal(error) => tracing::warn!(system, type_id, %error, "scheduled NPC spawn failed"),
                                    crate::services::CallError::User { msg, .. } => tracing::warn!(system, type_id, message = %msg, "scheduled NPC spawn refused"),
                                }
                            }
                        });
                    }
                });
            }).await.is_some())
        }
    })?)?;
    env.set("world", table)
}

#[cfg(test)]
mod tests;
