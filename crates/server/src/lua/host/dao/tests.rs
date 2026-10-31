use super::*;

#[test]
fn boundary_rejects_invalid_ids_amounts_and_recipient_shapes() {
    assert!(positive(0).is_err());
    assert!(positive(-1).is_err());
    assert_eq!(positive(7).unwrap(), 7);
    for value in [0.0, -1.0, f64::NAN, f64::INFINITY] { assert!(amount(value).is_err()); }
    assert_eq!(amount(1.5).unwrap(), 1.5);
    let lua = Lua::new();
    for source in ["{[2]=1}", "{[1]=1, x=2}", "{0}"] {
        let table = lua.load(format!("return {source}")).eval::<Table>().unwrap();
        assert!(ids(table).is_err(), "{source}");
    }
    let table = lua.load("return {7,8}").eval::<Table>().unwrap();
    assert_eq!(ids(table).unwrap(), vec![7, 8]);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated test database with SDE"]
async fn registered_read_functions_query_real_dedicated_database() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let lua = Lua::new();
    let env = lua.create_table().unwrap();
    register(&lua, &env, &db, &sde).unwrap();
    let dao: Table = env.get("dao").unwrap();
    let items: Table = dao.get("items").unwrap();
    for name in ["by_owner", "by_location"] {
        let function: mlua::Function = items.get(name).unwrap();
        let rows: Table = function.call_async(i64::MAX).await.unwrap();
        assert_eq!(rows.raw_len(), 0);
    }
    let characters: Table = dao.get("characters").unwrap();
    for name in ["get", "name", "ship"] {
        let function: mlua::Function = characters.get(name).unwrap();
        assert!(matches!(function.call_async::<mlua::Value>(i64::MAX).await.unwrap(), mlua::Value::Nil));
    }
    let skills: Table = dao.get("skills").unwrap();
    let levels: mlua::Function = skills.get("levels").unwrap();
    assert_eq!(levels.call_async::<Table>(i64::MAX).await.unwrap().raw_len(), 0);
    let corps: Table = dao.get("corps").unwrap();
    let corp: mlua::Function = corps.get("of").unwrap();
    assert!(matches!(corp.call_async::<mlua::Value>(i64::MAX).await.unwrap(), mlua::Value::Nil));
    let standings: Table = dao.get("standings").unwrap();
    let get: mlua::Function = standings.get("get").unwrap();
    assert!(matches!(get.call_async::<mlua::Value>((i64::MAX, i64::MAX)).await.unwrap(), mlua::Value::Nil));
}
