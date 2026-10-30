//! Shared item transfer and stack operations using `game.items` and `game.characters`.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Transfer policy, transactions, refusals, notifications and World are caller concerns.
use crate::db::{executor::Executor, Error};
use tokio_postgres::Row;
pub(crate) struct Stack { pub item_id: i64, pub quantity: i32 }
impl Stack { fn from_row(r: &Row) -> Self { Self { item_id: r.get("item_id"), quantity: r.get("quantity") } } }
struct ItemId { item_id: i64 }
impl ItemId { fn from_row(r: &Row) -> Self { Self { item_id: r.get("item_id") } } }
struct Station { station_id: Option<i64> }
impl Station { fn from_row(r: &Row) -> Self { Self { station_id: r.get("station_id") } } }
pub(crate) async fn take(ex: &(impl Executor + ?Sized), id: i64, quantity: i32) -> Result<(), Error> { ex.execute("UPDATE game.items SET quantity = quantity - $2 WHERE item_id = $1", &[&id, &quantity]).await?; Ok(()) }
pub(crate) async fn stack(ex: &(impl Executor + ?Sized), location: i64, flag: i32, type_id: i32, owner: i64) -> Result<Option<Stack>, Error> { Ok(ex.query_opt("SELECT item_id, quantity FROM game.items WHERE location_id = $1 AND flag_id = $2 AND type_id = $3 AND owner_id = $4 AND NOT singleton LIMIT 1", &[&location, &flag, &type_id, &owner]).await?.as_ref().map(Stack::from_row)) }
pub(crate) async fn add(ex: &(impl Executor + ?Sized), id: i64, quantity: i32) -> Result<(), Error> { ex.execute("UPDATE game.items SET quantity = quantity + $2 WHERE item_id = $1", &[&id, &quantity]).await?; Ok(()) }
pub(crate) async fn insert(ex: &(impl Executor + ?Sized), type_id: i32, owner: i64, location: i64, flag: i32, quantity: i32) -> Result<i64, Error> { let r = ex.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity) VALUES ($1, $2, $3, $4, $5) RETURNING item_id", &[&type_id, &owner, &location, &flag, &quantity]).await?; Ok(ItemId::from_row(&r).item_id) }
pub(crate) async fn docked(ex: &(impl Executor + ?Sized), owner: i64) -> Result<Option<i64>, Error> { Ok(ex.query_opt("SELECT station_id FROM game.characters WHERE character_id = $1", &[&owner]).await?.as_ref().and_then(|r| Station::from_row(r).station_id)) }
pub(crate) async fn hand_over(ex: &(impl Executor + ?Sized), ids: &[i64], owner: i64, location: i64, flag: i32) -> Result<(), Error> { ex.execute("UPDATE game.items SET owner_id = $2, location_id = $3, flag_id = $4 WHERE item_id = ANY($1)", &[&ids, &owner, &location, &flag]).await?; ex.execute("UPDATE game.items SET owner_id = $2 WHERE location_id = ANY($1)", &[&ids, &owner]).await?; Ok(()) }
