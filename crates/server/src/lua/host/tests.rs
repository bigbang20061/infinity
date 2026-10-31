use super::*;

#[test]
fn config_is_converted_recursively() {
    let lua = Lua::new();
    let config: toml::Value = toml::from_str("name = 'sample'\nvalues = [1, 2]\n[section]\nenabled = true").unwrap();
    let Value::Table(table) = config_value(&lua, &config).unwrap() else { panic!("expected table") };
    assert_eq!(table.get::<String>("name").unwrap(), "sample");
    let values: Table = table.get("values").unwrap();
    assert_eq!(values.get::<i64>(2).unwrap(), 2);
    let section: Table = table.get("section").unwrap();
    assert!(section.get::<bool>("enabled").unwrap());
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated test database with SDE"]
async fn sde_and_clocks_use_real_resources() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let world = World::new(db, sde.clone(), 1);
    let lua = Lua::new();
    let env = lua.create_table().unwrap();
    super::sde::register(&lua, &env, &sde).unwrap();
    super::time::register(&lua, &env, &world).unwrap();
    let data: Table = env.get("sde").unwrap();
    let type_data = sde.types.iter().find(|t| t.group_id.and_then(|g| sde.types.group(g)).and_then(|g| g.category_id).is_some()).expect("SDE type with group and category");
    let type_fn: mlua::Function = data.get("type").unwrap();
    let row: Table = type_fn.call(type_data.type_id).unwrap();
    assert_eq!(row.get::<i32>("type_id").unwrap(), type_data.type_id);
    let group_id = type_data.group_id.unwrap();
    let group_fn: mlua::Function = data.get("group").unwrap();
    let group: Table = group_fn.call(group_id).unwrap();
    let category_id: i32 = group.get("category_id").unwrap();
    let category_fn: mlua::Function = data.get("category").unwrap();
    let _: Table = category_fn.call(category_id).unwrap();
    let attribute_fn: mlua::Function = data.get("attribute").unwrap();
    let (type_id, attribute_id, expected) = sde.types.iter().find_map(|t| sde.types.attributes(t.type_id).next().map(|(a, v)| (t.type_id, a, v))).expect("SDE attribute");
    assert_eq!(attribute_fn.call::<f64>((type_id, attribute_id)).unwrap(), expected);
    let system = sde.map.systems().next().expect("SDE system");
    let system_fn: mlua::Function = data.get("system").unwrap();
    let row: Table = system_fn.call(system.solar_system_id).unwrap();
    assert_eq!(row.get::<i64>("solar_system_id").unwrap(), system.solar_system_id);
    let time: Table = env.get("time").unwrap();
    let ms: mlua::Function = time.get("now_ms").unwrap();
    assert!(ms.call::<i64>(()).unwrap() > 0);
    let filetime: mlua::Function = time.get("now_filetime").unwrap();
    assert!(filetime.call::<i64>(()).unwrap() > 0);
    let tick: mlua::Function = time.get("now_tick").unwrap();
    assert!(tick.call_async::<i64>(system.solar_system_id).await.is_err());
    world.system(system.solar_system_id).await.unwrap();
    assert!(tick.call_async::<i64>(system.solar_system_id).await.unwrap() > 0);
}
