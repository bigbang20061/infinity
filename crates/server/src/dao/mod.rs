//! Plain data access for accounts, alliances, asteroids, bills, bookmarks, calendar,
//! characters, contracts, corporations, exploration, industry, insurance, items,
//! killmails, mail, market, missions, NPE, offices, orgs, skills, store and wallets.
//! Aggregates read and write `game` tables and use typed `sde` and `sde_native`
//! static data; individual headers describe their sources and coverage.
//! This layer has no direct client mirror or wire contract; owning services supply those.
//! Callers own policy, transactions, Outbox effects, refusals, notifications and World.

pub(crate) mod accounts;
pub(crate) mod alliances;
pub(crate) mod asteroids;
pub(crate) mod bills;
pub(crate) mod bookmarks;
pub(crate) mod calendar;
pub(crate) mod characters;
pub(crate) mod contracts;
pub(crate) mod corporations;
pub(crate) mod exploration;
pub(crate) mod industry;
pub(crate) mod insurance;
pub(crate) mod items;
pub(crate) mod jobs;
pub(crate) mod killmails;
pub(crate) mod lua_store;
pub(crate) mod mail;
pub(crate) mod market;
pub(crate) mod missions;
pub(crate) mod npe;
pub(crate) mod offices;
pub(crate) mod orgs;
pub(crate) mod sde;
pub(crate) mod skills;
pub(crate) mod slash;
pub(crate) mod store;
pub(crate) mod wallets;
