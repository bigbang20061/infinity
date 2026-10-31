use super::*;

#[test]
fn all_log_levels_accept_text_and_reject_tables() {
    let lua = Lua::new();
    let env = lua.create_table().unwrap();
    register(&lua, &env, "log-test.lua").unwrap();
    let log: Table = env.get("log").unwrap();
    for name in ["info", "warn", "debug"] {
        let function: mlua::Function = log.get(name).unwrap();
        function.call::<()>("diagnostic").unwrap();
        assert!(function.call::<()>(lua.create_table().unwrap()).is_err());
    }
}
