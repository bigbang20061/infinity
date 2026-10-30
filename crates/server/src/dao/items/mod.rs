//! Item loading, inventory, creation, transfers, quantities, positions, charges,
//! ship, clone, combat, dogma, fitting, hacking, mining and social persistence.
//! Sources include `game.items`, `game.characters`, `game.module_charges`,
//! `game.pending_reloads`, `game.weapon_banks`, `game.fittings`, `game.blueprints`,
//! `game.industry_jobs`, `game.market_orders` and SDE type, attribute and station data.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Policy, transactions, refusals, notifications and World are caller concerns.

pub(crate) mod charges;
pub(crate) mod clones;
pub(crate) mod combat;
pub(crate) mod creation;
pub(crate) mod dogma;
pub(crate) mod fit;
pub(crate) mod fitting;
pub(crate) mod hacking;
pub(crate) mod inventory;
pub(crate) mod loading;
pub(crate) mod mining;
pub(crate) mod pending_reloads;
pub(crate) mod positions;
pub(crate) mod quantities;
pub(crate) mod ship;
pub(crate) mod social;
pub(crate) mod transfers;
