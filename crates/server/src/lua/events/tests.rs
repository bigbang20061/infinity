use super::*;

#[test]
fn payload_contains_typed_named_scalars() -> mlua::Result<()> {
    let lua = Lua::new();
    let event = GameEvent::MarketOrderPlaced { char_id: 4, order_id: 5, type_id: 6, quantity: 7, price: 1.5, is_buy: true };
    let table = event.to_lua(&lua)?;
    assert_eq!(table.get::<String>("kind")?, "MarketOrderPlaced");
    assert_eq!(table.get::<i64>("char_id")?, 4);
    assert_eq!(table.get::<i32>("quantity")?, 7);
    assert_eq!(table.get::<f64>("price")?, 1.5);
    assert!(table.get::<bool>("is_buy")?);
    Ok(())
}
