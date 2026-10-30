//! Character creation, selection, membership, attributes and name persistence.
//! Uses `game.characters`, `game.items`, `game.corporations`, `game.corp_titles` and `sde.chr_bloodlines`.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Session policy, transactions, refusals, notifications and World are caller concerns.

pub(crate) mod attributes;
pub(crate) mod creation;
pub(crate) mod crimewatch;
pub(crate) mod members;
pub(crate) mod persistence;
pub(crate) mod roles;
pub(crate) mod routing;
pub(crate) mod social;
pub(crate) mod selection;
pub(crate) mod standings;
