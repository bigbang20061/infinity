//! SQL expression combining character role bits with held titles in `game.corp_titles`.
//! The caller supplies the character row; this DAO has no direct client mirror or wire contract.
//! Owning services supply client contracts and handle role policy, transactions, refusals, notifications and World.

/// Server title model: union the member's role bits with the roles of all held titles.
pub(crate) const EFFECTIVE_ROLES_SQL: &str = "(ch.corp_roles | COALESCE((SELECT bit_or(t.roles) FROM game.corp_titles t
     WHERE t.corporation_id = ch.corporation_id AND ch.corp_title_mask & t.title_id <> 0), 0))";
