//! Skill presence, training, queue and plan persistence using `game.skills`,
//! `game.skill_queue`, `game.skill_plans` and `sde.type_attributes`.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Training policy, transactions, refusals, notifications and World are caller concerns.

pub(crate) mod plans;
pub(crate) mod queue;
pub(crate) mod selection;
pub(crate) mod training;
