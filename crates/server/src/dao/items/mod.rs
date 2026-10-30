//! Item loading, inventory, charges, positions, quantities and transfer persistence.
//! Uses `game.items`, `game.characters`, `game.module_charges`, `game.weapon_banks`,
//! `sde.inv_types` and `sde.type_attributes`.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Transactions, refusals, notifications and World are caller concerns.

pub(crate) mod charges;
pub(crate) mod clones;
pub(crate) mod combat;
pub(crate) mod creation;
pub(crate) mod dogma;
pub(crate) mod inventory;
pub(crate) mod loading;
pub(crate) mod positions;
pub(crate) mod quantities;
pub(crate) mod social;
pub(crate) mod transfers;
