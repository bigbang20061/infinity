//! Market startup seeding and repricing using `game.market_orders`, `game.retail_prices`,
//! `sde_native.schools`, `sde.sta_stations`, `sde.inv_type_materials` and `sde.inv_types`.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Matching policy, transactions, refusals, notifications and World are caller concerns.

pub(crate) mod startup;
