//! Rookie ship creation in `game.items`.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Ship selection, character association, transactions, refusals, notifications and World are caller concerns.
use crate::db::{self, executor::Executor};
use tokio_postgres::Row;
struct ItemId { item_id: i64 }
impl ItemId { fn from_row(r: &Row) -> Self { Self { item_id: r.get("item_id") } } }
pub(crate) async fn rookie_ship(ex: &(impl Executor + ?Sized), ship_type: i32, character_id: i64, station_id: i64, flag: i32) -> Result<i64, db::Error> {
    Ok(ItemId::from_row(&ex.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton)
             VALUES ($1, $2, $3, $4, 1, true) RETURNING item_id", &[&ship_type, &character_id, &station_id, &flag]).await?).item_id)
}
