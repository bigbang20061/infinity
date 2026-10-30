//! Ship item persistence in game.items and game.module_charges, with SDE type attributes.
//! Ship services own movement, transactions and notifications; no client wire contract lives here.
//! Shared item type lookup and positioned creation live in `items::inventory` and `items::creation`.
//! Reload start, completion, return, recovery and ship loss lock the ship item row
//! as their first transaction statement, before acquiring child rows in their usual order.
use crate::db::{self, executor::Executor};
use tokio_postgres::Row;
use super::{charges::Charge, combat, inventory};
pub(crate) struct BallAttributes { pub type_id:i32, pub radius:Option<f64>, pub mass_attr:Option<f64>, pub velocity:Option<f64>, pub agility:Option<f64>, pub warp_mult:Option<f64>, pub base_warp:Option<f64>, pub mass:Option<f64> }
impl BallAttributes { fn from_row(r:&Row)->Self { Self { type_id:r.get("type_id"), radius:r.get("radius"), mass_attr:r.get("mass_attr"), velocity:r.get("velocity"), agility:r.get("agility"), warp_mult:r.get("warp_mult"), base_warp:r.get("base_warp"), mass:r.get("mass") } } }
pub(crate) async fn lock_reload_ship(ex:&(impl Executor+?Sized), ship:i64)->Result<bool,db::Error> {
    Ok(ex.query_opt("SELECT item_id FROM game.items WHERE item_id = $1 FOR UPDATE", &[&ship]).await?.is_some())
}
pub(crate) async fn ball(ex:&(impl Executor+?Sized), ship:i64, mass:i32, velocity:i32, agility:i32, warp:i32, base:i32)->Result<BallAttributes,db::Error> {
 Ok(BallAttributes::from_row(&ex.query_one("SELECT i.type_id, t.radius,
                    (SELECT value FROM sde.type_attributes WHERE type_id = i.type_id AND attribute_id = $2) AS mass_attr,
                    (SELECT value FROM sde.type_attributes WHERE type_id = i.type_id AND attribute_id = $3) AS velocity,
                    (SELECT value FROM sde.type_attributes WHERE type_id = i.type_id AND attribute_id = $4) AS agility,
                    (SELECT value FROM sde.type_attributes WHERE type_id = i.type_id AND attribute_id = $5) AS warp_mult,
                    (SELECT value FROM sde.type_attributes WHERE type_id = i.type_id AND attribute_id = $6) AS base_warp,
                    t.mass
             FROM game.items i JOIN sde.inv_types t USING (type_id) WHERE i.item_id = $1", &[&ship,&mass,&velocity,&agility,&warp,&base]).await?))
}
pub(crate) async fn undock(ex:&(impl Executor+?Sized), ship:i64, system:i64, pos:[f64;3])->Result<u64,db::Error> { ex.execute("UPDATE game.items SET location_id = $2, flag_id = 0, x = $3, y = $4, z = $5 WHERE item_id = $1", &[&ship,&system,&pos[0],&pos[1],&pos[2]]).await }
pub(crate) async fn dock(ex:&(impl Executor+?Sized), ship:i64, station:i64, flag:i32)->Result<u64,db::Error> { ex.execute("UPDATE game.items SET location_id = $2, flag_id = $3, x = NULL, y = NULL, z = NULL WHERE item_id = $1", &[&ship,&station,&flag]).await }
pub(crate) async fn position(ex:&(impl Executor+?Sized), ship:i64, system:i64, pos:[f64;3])->Result<u64,db::Error> { ex.execute("UPDATE game.items SET location_id = $2, x = $3, y = $4, z = $5 WHERE item_id = $1", &[&ship,&system,&pos[0],&pos[1],&pos[2]]).await }
pub(crate) struct LostItem { pub type_id:i32, pub flag_id:i32, pub singleton:bool, pub quantity:i32 }
impl LostItem { fn from_row(r:&Row)->Self { Self { type_id:r.get("type_id"), flag_id:r.get("flag_id"), singleton:r.get("singleton"), quantity:r.get("quantity") } } }
pub(crate) async fn lost_items(ex:&(impl Executor+?Sized), ship:i64)->Result<Vec<LostItem>,db::Error> { Ok(ex.query("SELECT type_id, flag_id, singleton, quantity FROM game.items WHERE location_id = $1", &[&ship]).await?.iter().map(LostItem::from_row).collect()) }
pub(crate) async fn lost_charges(ex:&(impl Executor+?Sized), ship:i64)->Result<Vec<Charge>,db::Error> { Ok(ex.query("SELECT type_id, flag_id, quantity FROM game.module_charges WHERE ship_id = $1", &[&ship]).await?.iter().map(Charge::from_row).collect()) }
pub(crate) async fn delete_lost(ex:&(impl Executor+?Sized), ship:i64)->Result<(),db::Error> {
    ex.execute("DELETE FROM game.items WHERE location_id = $1", &[&ship]).await?;
    ex.execute("DELETE FROM game.module_charges WHERE ship_id = $1", &[&ship]).await?;
    combat::clear_banks(ex, ship).await?;
    inventory::delete(ex, ship).await?;
    Ok(())
}
