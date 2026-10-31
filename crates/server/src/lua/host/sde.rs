//! Read-only snapshots from the startup SDE. Client data consumers include the
//! inventory and dogma services. No runtime SQL or mutable static data is exposed.

use std::sync::Arc;
use mlua::{Lua, Result, Table};
use crate::sde::Sde;

pub(super) fn register(lua: &Lua, env: &Table, sde: &Arc<Sde>) -> Result<()> {
    let table = lua.create_table()?;
    let data = Arc::clone(sde);
    table.set("type", lua.create_function(move |lua, id: i32| {
        data.types.get(id).map(|item| {
            let row = lua.create_table()?;
            row.set("type_id", item.type_id)?;
            row.set("group_id", item.group_id)?;
            row.set("category_id", item.category_id)?;
            row.set("name", item.name.clone())?;
            Ok(row)
        }).transpose()
    })?)?;
    let data = Arc::clone(sde);
    table.set("attribute", lua.create_function(move |_, (id, attribute): (i32, i32)| Ok(data.types.attribute(id, attribute)))?)?;
    let data = Arc::clone(sde);
    table.set("group", lua.create_function(move |lua, id: i32| {
        data.types.group(id).map(|group| {
            let row = lua.create_table()?;
            row.set("category_id", group.category_id)?;
            row.set("name", group.name.clone())?;
            Ok(row)
        }).transpose()
    })?)?;
    let data = Arc::clone(sde);
    table.set("category", lua.create_function(move |lua, id: i32| {
        data.types.category(id).map(|category| {
            let row = lua.create_table()?;
            row.set("name", category.name.clone())?;
            Ok(row)
        }).transpose()
    })?)?;
    let data = Arc::clone(sde);
    table.set("system", lua.create_function(move |lua, id: i64| {
        data.map.system(id).map(|system| {
            let row = lua.create_table()?;
            row.set("solar_system_id", system.solar_system_id)?;
            row.set("name", system.name.clone())?;
            row.set("constellation_id", system.constellation_id)?;
            Ok(row)
        }).transpose()
    })?)?;
    env.set("sde", table)
}
