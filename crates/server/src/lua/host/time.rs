//! Script access to FILETIME, wall time and the running system's destiny clock.
//! Destiny time comes from the system actor, not from a client or database table.
//! A stopped system has no destiny clock and returns an error.

use std::sync::Arc;
use mlua::{Lua, Result, Table};
use crate::space::World;

pub(super) fn register(lua: &Lua, env: &Table, world: &Arc<World>) -> Result<()> {
    let table = lua.create_table()?;
    table.set("now_filetime", lua.create_function(|_, ()| Ok(crate::client::filetime_now()))?)?;
    table.set("now_ms", lua.create_function(|_, ()| Ok(crate::space::now_ms()))?)?;
    let world = Arc::clone(world);
    table.set("now_tick", lua.create_async_function(move |_, system: i64| {
        let world = Arc::clone(&world);
        async move {
            world.with(system, |s| s.current_time()).await
                .ok_or_else(|| mlua::Error::runtime(format!("system {system} is not running")))
        }
    })?)?;
    env.set("time", table)
}
