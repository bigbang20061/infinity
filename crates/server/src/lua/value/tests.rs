use super::*;

#[test]
fn constructors_preserve_wire_types_and_reject_raw_values() -> mlua::Result<()> {
    let lua = Lua::new();
    let env = lua.create_table()?;
    register(&lua, &env)?;
    let values: (mlua::Value, mlua::Value, mlua::Value, mlua::Value) = lua.load("return eve.str('x'), eve.unicode('x'), eve.tuple{eve.int(1)}, eve.list{eve.int(1)}").set_environment(env.clone()).eval()?;
    let (a, b, c, d) = values;
    let a = from_lua(a)?;
    let b = from_lua(b)?;
    let c = from_lua(c)?;
    let d = from_lua(d)?;
    assert_ne!(marshal::dumps(&a), marshal::dumps(&b));
    assert_ne!(marshal::dumps(&c), marshal::dumps(&d));
    let raw: mlua::Value = lua.load("return {}").eval()?;
    assert!(from_lua(raw).unwrap_err().to_string().contains("eve"));
    Ok(())
}

#[test]
fn containers_and_accessors() -> mlua::Result<()> {
    let lua = Lua::new();
    let env = lua.create_table()?;
    register(&lua, &env)?;
    let result: (i64, String, bool, usize) = lua.load("local d=eve.dict{x=eve.int(42), s=eve.unicode('text')}; return d:get('x'):int(), d:get('s'):text(), eve.none:is_none(), eve.list{eve.bool(true)}:len()").set_environment(env).eval()?;
    assert_eq!(result, (42, "text".into(), true, 1));
    Ok(())
}

#[test]
fn all_constructors_survive_marshal_roundtrip() -> mlua::Result<()> {
    let lua = Lua::new();
    let env = lua.create_table()?;
    register(&lua, &env)?;
    let values: Table = lua.load(r#"
        local d = eve.descriptor{{"itemID","i8"},{"name","str"}}
        return {eve.none, eve.bool(true), eve.int(42), eve.float(1.5), eve.str("id"),
            eve.unicode("文本"), eve.tuple{eve.int(1)}, eve.list{eve.int(2)},
            eve.dict{x=eve.int(3)}, eve.keyval{x=eve.int(4)},
            eve.instance("example.Class", {x=eve.int(5)}),
            eve.reduce("example.make", {eve.int(6)}, eve.none),
            eve.rowset({"id"}, {{eve.int(7)}}),
            d, eve.crowset(d, {{eve.int(8), eve.str("name")}})}
    "#).set_environment(env).eval()?;
    for item in values.sequence_values::<mlua::Value>() {
        let value = from_lua(item?)?;
        let encoded = marshal::dumps(&value);
        let decoded = marshal::loads(&encoded).unwrap();
        assert_eq!(marshal::dumps(&decoded), encoded);
    }
    Ok(())
}

#[test]
fn malformed_sequences_are_rejected() -> mlua::Result<()> {
    let lua = Lua::new();
    let env = lua.create_table()?;
    register(&lua, &env)?;
    for expression in [
        "eve.list{[1]=eve.int(1),[3]=eve.int(3)}",
        "eve.tuple{eve.int(1),extra=eve.int(2)}",
        "eve.reduce('x', {[2]=eve.int(1)}, eve.none)",
        "eve.rowset({'a','b'}, {{eve.int(1)}})",
        "eve.rowset({[2]='a'}, {})",
        "eve.descriptor{{'a','i8',extra='x'}}",
        "eve.crowset(eve.descriptor{{'a','i8'}}, {[2]={eve.int(1)}})",
    ] {
        assert!(lua.load(expression).set_environment(env.clone()).eval::<mlua::Value>().is_err(), "{expression}");
    }
    Ok(())
}
