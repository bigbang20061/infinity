//! Corporation persistence modules for registry, roles and militia enlistment.
//! These have no direct client wire mirror; their owning services support
//! `eve/client/script/ui/services/corporation` and enlistment calls.
//! Sources include `game.corporations`, `game.characters`, `game.corp_applications`,
//! `game.corp_titles`, `game.corp_enlistment_factions` and SDE corporation data.
//! Service validation, wire conversion, transactions and notifications are not handled here;
//! corporation shares are not supported.

pub(crate) mod registry;
pub(crate) mod dispatch;
pub(crate) mod roles;
