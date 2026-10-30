//! Combat item, ammunition and wreck persistence in game tables.
//! Combat policy, transactions, refusals, effects and notifications remain with services.
//! Positioned creation and cargo stack insertion live in `items::creation` and `items::transfers`.
use crate::db::{executor::Executor, Error};
use super::{charges::{Loaded, Slave}, transfers};
use tokio_postgres::Row;
pub(crate) struct Loot { pub type_id: i32 }
impl Loot { fn from_row(row: &Row) -> Self { Self { type_id: row.get("type_id") } } }

pub(crate) async fn delete_charge(ex: &(impl Executor + ?Sized), ship: i64, flag: i32) -> Result<u64, Error> {
    ex.execute("DELETE FROM game.module_charges WHERE ship_id = $1 AND flag_id = $2", &[&ship, &flag]).await
}
pub(crate) async fn consume_charge(ex: &(impl Executor + ?Sized), ship: i64, flag: i32) -> Result<Option<Loaded>, Error> {
    Ok(ex.query_opt("UPDATE game.module_charges SET quantity = quantity - 1 WHERE ship_id = $1 AND flag_id = $2 AND quantity > 0 RETURNING type_id, quantity", &[&ship, &flag]).await?.as_ref().map(Loaded::from_row))
}
pub(crate) async fn set_online(ex: &(impl Executor + ?Sized), item: i64, ship: i64, online: bool) -> Result<u64, Error> {
    ex.execute("UPDATE game.items SET online = $3 WHERE item_id = $1 AND location_id = $2 AND online <> $3", &[&item, &ship, &online]).await
}
pub(crate) async fn loot(ex: &(impl Executor + ?Sized), bounty: f64) -> Result<Vec<Loot>, Error> {
    Ok(ex.query("SELECT type_id FROM game.retail_prices WHERE average BETWEEN 1000 AND GREATEST($1::float8, 50000)", &[&bounty]).await?.iter().map(Loot::from_row).collect())
}
pub(crate) async fn insert_loot(ex: &(impl Executor + ?Sized), kind: i32, owner: i64, wreck: i64, quantity: i32) -> Result<u64, Error> {
    ex.execute("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity) VALUES ($1, $2, $3, 0, $4)", &[&kind, &owner, &wreck, &quantity]).await
}

pub(crate) struct FittedWeapon { pub item_id: i64, pub type_id: i32 }
impl FittedWeapon { fn from_row(row: &Row) -> Self { Self { item_id: row.get("item_id"), type_id: row.get("type_id") } } }
pub(crate) async fn link(ex: &(impl Executor + ?Sized), ship: i64, master: i64, slave: i64) -> Result<(), Error> {
    ex.execute("UPDATE game.weapon_banks SET master_id = $2 WHERE ship_id = $1 AND master_id = $3", &[&ship, &master, &slave]).await?;
    ex.execute("INSERT INTO game.weapon_banks (ship_id, master_id, slave_id) VALUES ($1, $2, $3) ON CONFLICT (slave_id) DO UPDATE SET master_id = EXCLUDED.master_id", &[&ship, &master, &slave]).await?;
    Ok(())
}
pub(crate) async fn peel_and_link(ex: &(impl Executor + ?Sized), ship: i64, master: i64, slave: i64) -> Result<(), Error> {
    ex.execute("DELETE FROM game.weapon_banks WHERE ship_id = $1 AND slave_id = $2", &[&ship, &slave]).await?;
    ex.execute("INSERT INTO game.weapon_banks (ship_id, master_id, slave_id) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING", &[&ship, &master, &slave]).await?;
    Ok(())
}
pub(crate) async fn clear_banks(ex: &(impl Executor + ?Sized), ship: i64) -> Result<u64, Error> {
    ex.execute("DELETE FROM game.weapon_banks WHERE ship_id = $1", &[&ship]).await
}
pub(crate) async fn fitted_weapons(ex: &(impl Executor + ?Sized), ship: i64, first: i32, last: i32) -> Result<Vec<FittedWeapon>, Error> {
    Ok(ex.query("SELECT item_id, type_id FROM game.items WHERE location_id = $1 AND flag_id BETWEEN $2 AND $3 AND online ORDER BY flag_id", &[&ship, &first, &last]).await?.iter().map(FittedWeapon::from_row).collect())
}
pub(crate) async fn unlink_last(ex: &(impl Executor + ?Sized), ship: i64, master: i64) -> Result<Option<i64>, Error> {
    Ok(ex.query_opt("DELETE FROM game.weapon_banks WHERE slave_id = (SELECT max(slave_id) FROM game.weapon_banks WHERE ship_id = $1 AND master_id = $2) RETURNING slave_id", &[&ship, &master]).await?.as_ref().map(Slave::from_row).map(|r| r.slave_id))
}
pub(crate) async fn delete_reserved(ex: &(impl Executor + ?Sized), ship: i64, rows: &std::collections::BTreeMap<i32, (i32, i32, i64)>) -> Result<bool, Error> {
    for (&flag, &(kind, quantity, _)) in rows {
        if ex.execute("DELETE FROM game.module_charges WHERE ship_id = $1 AND flag_id = $2 AND type_id = $3 AND quantity = $4", &[&ship, &flag, &kind, &quantity]).await? == 0 { return Ok(false); }
    }
    Ok(true)
}
pub(crate) async fn insert_cargo(ex: &(impl Executor + ?Sized), ship: i64, flag: i32, rows: &std::collections::BTreeMap<(i64, i32), i32>) -> Result<Vec<i64>, Error> {
    let mut ids = Vec::with_capacity(rows.len());
    for (&(owner, kind), &quantity) in rows {
        ids.push(transfers::insert(ex, kind, owner, ship, flag, quantity).await?);
    }
    Ok(ids)
}
pub(crate) async fn apply_loads(ex: &(impl Executor + ?Sized), ship: i64, loads: &[(i64, i32, i32, i32, i32)]) -> Result<(), Error> {
    for &(_, flag, kind, quantity, _) in loads {
        ex.execute("INSERT INTO game.module_charges (ship_id, flag_id, type_id, quantity) VALUES ($1, $2, $3, $4)
                         ON CONFLICT (ship_id, flag_id) DO UPDATE SET type_id = EXCLUDED.type_id, quantity = EXCLUDED.quantity", &[&ship, &flag, &kind, &quantity]).await?;
    }
    Ok(())
}
