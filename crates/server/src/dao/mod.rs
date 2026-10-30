//! Data access for accounts, bills, characters, contracts, insurance, items, market,
//! offices, skills, store and wallets.
//! Uses `game.accounts`, `game.characters`, `game.corporations`, `game.corp_titles`,
//! `game.items`, `game.module_charges`, `game.weapon_banks`, `game.market_orders`,
//! `game.retail_prices`, `game.skills`, `game.skill_queue`, `game.skill_plans` and
//! `game.corp_wallets`; additional aggregates use contract, bill, insurance, office,
//! redeem-token and store-purchase data. Static sources include `sde.chr_bloodlines`, `sde.inv_types`,
//! `sde.type_attributes`, `sde.sta_stations`, `sde.inv_type_materials` and `sde_native.schools`.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Transactions, refusals, notifications and World are caller concerns.

pub(crate) mod accounts;
pub(crate) mod bills;
pub(crate) mod bookmarks;
pub(crate) mod calendar;
pub(crate) mod characters;
pub(crate) mod contracts;
pub(crate) mod insurance;
pub(crate) mod items;
pub(crate) mod mail;
pub(crate) mod market;
pub(crate) mod offices;
pub(crate) mod skills;
pub(crate) mod store;
pub(crate) mod wallets;
