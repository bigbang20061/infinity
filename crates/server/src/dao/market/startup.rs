//! Startup market seeding and retail price persistence using `game.market_orders`,
//! `game.retail_prices`, `sde_native.schools`, `sde.sta_stations`,
//! `sde.inv_type_materials` and `sde.inv_types`; the seed query is `sql/market_seed.sql`.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Startup orchestration, transactions, refusals, notifications and World are caller concerns.
use crate::db::{self, executor::Executor};
use tokio_postgres::Row;
const MARKET_SEED: &str = include_str!("../../../sql/market_seed.sql");
struct Seeded { seeded: bool }
impl Seeded { fn from_row(r: &Row) -> Self { Self { seeded: r.get("seeded") } } }
pub(crate) async fn retail_prices(ex: &(impl Executor + ?Sized), ids: &[i32], avg: &[f64], adj: &[f64]) -> Result<u64, db::Error> {
    ex.execute("INSERT INTO game.retail_prices (type_id, average, adjusted)
             SELECT * FROM unnest($1::int[], $2::float8[], $3::float8[])
             ON CONFLICT (type_id) DO UPDATE SET average = EXCLUDED.average, adjusted = EXCLUDED.adjusted", &[&ids, &avg, &adj]).await
}
pub(crate) async fn seeded(ex: &(impl Executor + ?Sized)) -> Result<bool, db::Error> {
    Ok(Seeded::from_row(&ex.query_one("SELECT EXISTS (SELECT 1 FROM game.market_orders WHERE char_id IS NULL) AS seeded", &[]).await?).seeded)
}
pub(crate) async fn seed(ex: &(impl Executor + ?Sized)) -> Result<u64, db::Error> { ex.execute(MARKET_SEED, &[]).await }
pub(crate) async fn bids_seeded(ex: &(impl Executor + ?Sized)) -> Result<bool, db::Error> {
    Ok(Seeded::from_row(&ex.query_one("SELECT EXISTS (SELECT 1 FROM game.market_orders WHERE char_id IS NULL AND bid) AS seeded", &[]).await?).seeded)
}
pub(crate) async fn seed_bids(ex: &(impl Executor + ?Sized)) -> Result<u64, db::Error> {
    ex.execute("INSERT INTO game.market_orders (type_id, station_id, solar_system_id, constellation_id, region_id,
                                                     price, vol_entered, vol_remaining, bid, range, duration)
                     SELECT type_id, station_id, solar_system_id, constellation_id, region_id,
                            GREATEST(round((price * $1::float8)::numeric, 2), 0.01)::float8, 1000000, 1000000, true, 32767, 365
                     FROM game.market_orders WHERE char_id IS NULL AND NOT bid", &[&crate::config::game().market.npc_buy_ratio]).await
}
pub(crate) async fn reprice(ex: &(impl Executor + ?Sized)) -> Result<u64, db::Error> {
    ex.execute("UPDATE game.market_orders o
                 SET price = GREATEST(round((r.average * CASE WHEN o.bid THEN $1::float8 ELSE 1 END)::numeric, 2), 0.01)::float8
                 FROM game.retail_prices r
                 WHERE o.char_id IS NULL AND o.type_id = r.type_id AND r.average > 0
                   AND o.price <> GREATEST(round((r.average * CASE WHEN o.bid THEN $1::float8 ELSE 1 END)::numeric, 2), 0.01)::float8", &[&crate::config::game().market.npc_buy_ratio]).await
}
