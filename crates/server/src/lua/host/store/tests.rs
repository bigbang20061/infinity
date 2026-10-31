use super::*;

#[test]
fn nested_values_round_trip_and_invalid_values_fail() {
    let lua = Lua::new();
    for source in ["true", "42", "1.5", "'text'", "{name='x', nested={1,2}}"] {
        let value: Value = lua.load(format!("return {source}")).eval().unwrap();
        let json = encode(value, "key", &mut HashSet::new(), &mut 65536, 0).unwrap();
        let restored = decode(&lua, json.clone(), "key", 0).unwrap();
        assert_eq!(encode(restored, "key", &mut HashSet::new(), &mut 65536, 0).unwrap(), json);
    }
    for source in ["nil", "function() end", "0/0", "1/0", "{[1]=1, x=2}", "{[2]=1}", "{[false]=1}", "(function() local t={} t.x=t return t end)()"] {
        let value: Value = lua.load(format!("return {source}")).eval().unwrap();
        let error = encode(value, "key", &mut HashSet::new(), &mut 65536, 0).unwrap_err().to_string();
        assert!(error.contains("key"), "{source}: {error}");
    }
    let deep: Value = lua.load("local t={} for i=1,70 do t={t} end return t").eval().unwrap();
    assert!(encode(deep, "deep", &mut HashSet::new(), &mut 65536, 0).is_err());
    let value: Value = lua.load("return '12345'").eval().unwrap();
    assert!(encode(value, "large", &mut HashSet::new(), &mut 6, 0).is_err());
}

#[test]
fn persisted_values_enforce_depth_and_name_the_key() {
    let lua = Lua::new();
    let mut value = serde_json::json!(true);
    for _ in 0..=MAX_DEPTH { value = serde_json::Value::Array(vec![value]); }
    assert!(decode(&lua, value, "deep-key", 0).unwrap_err().to_string().contains("deep-key"));
    assert!(decode(&lua, serde_json::Value::Null, "null-key", 0).unwrap_err().to_string().contains("null-key"));
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated test database"]
async fn registered_stores_are_bound_to_their_own_script() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = std::sync::Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let world = crate::space::World::new(db.clone(), sde.clone(), 1);
    let resources = super::super::Resources { db: db.clone(), sde, world, store_value_limit_bytes: 65536 };
    let lua = Lua::new();
    let a = lua.create_table().unwrap();
    let b = lua.create_table().unwrap();
    let suffix = format!("{}_{}", std::process::id(), crate::space::now_ms());
    let script_a = format!("fixture_a_{suffix}");
    let script_b = format!("fixture_b_{suffix}");
    let key = "isolation";
    assert!(lua_store::get(&db, &script_a, key).await.unwrap().is_none());
    assert!(lua_store::get(&db, &script_b, key).await.unwrap().is_none());
    super::super::register(&lua, &a, &script_a, &toml::Table::new(), &resources).unwrap();
    super::super::register(&lua, &b, &script_b, &toml::Table::new(), &resources).unwrap();
    assert!(a.get::<Option<Table>>("db").unwrap().is_none());
    assert!(b.get::<Option<Table>>("db").unwrap().is_none());
    let a: Table = a.get("store").unwrap();
    let b: Table = b.get("store").unwrap();
    let set: mlua::Function = a.get("set").unwrap();
    set.call_async::<()>((key, "alpha")).await.unwrap();
    let get_a: mlua::Function = a.get("get").unwrap();
    let get_b: mlua::Function = b.get("get").unwrap();
    assert_eq!(get_a.call_async::<String>(key).await.unwrap(), "alpha");
    assert!(matches!(get_b.call_async::<Value>(key).await.unwrap(), Value::Nil));
    let set_b: mlua::Function = b.get("set").unwrap();
    set_b.call_async::<()>((key, "beta")).await.unwrap();
    assert_eq!(get_a.call_async::<String>(key).await.unwrap(), "alpha");
    assert_eq!(get_b.call_async::<String>(key).await.unwrap(), "beta");
    let keys: mlua::Function = a.get("keys").unwrap();
    let listed: Table = keys.call_async("iso").await.unwrap();
    assert_eq!(listed.get::<String>(1).unwrap(), key);
    let delete_a: mlua::Function = a.get("delete").unwrap();
    let delete_b: mlua::Function = b.get("delete").unwrap();
    assert!(delete_a.call_async::<bool>(key).await.unwrap());
    assert!(delete_b.call_async::<bool>(key).await.unwrap());
    assert!(!delete_a.call_async::<bool>(key).await.unwrap());
    let oversized = serde_json::to_string(&"x".repeat(65537)).unwrap();
    lua_store::set(&db, &script_a, "oversized", &oversized).await.unwrap();
    assert!(get_a.call_async::<Value>("oversized").await.unwrap_err().to_string().contains("oversized"));
    assert!(delete_a.call_async::<bool>("oversized").await.unwrap());
    assert!(lua_store::keys(&db, &script_a, "").await.unwrap().is_empty());
    assert!(lua_store::keys(&db, &script_b, "").await.unwrap().is_empty());
}
