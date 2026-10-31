//! Skill service routing and shared mechanics, mirroring characterskills/util.py.
//! Uses game.skills, game.skill_queue, game.characters and startup SDE bloodline,
//! race and type attribute data through its submodules.
//! Effective character attributes use Fit modifiers; several skill handler features are not modelled.
use super::{db_err, keyval, Call, CallError, CallResult};
use crate::client::Client;
pub(crate) mod attributes;
pub(crate) mod formulas;
pub(crate) mod requirements;
pub(crate) mod starting;
mod skill_mgr2;
mod skill_handler;
mod training;
pub(crate) mod completion;
pub(crate) mod gm;
mod history;
mod free_points;
pub(crate) mod books;
mod trading;
mod respec;
#[cfg(test)]
mod tests;

/// Internal bound-object name for skillMgr2.GetMySkillHandler.
pub const SKILL_HANDLER: &str = "skillHandler";

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service == "skillMgr2" { skill_mgr2::dispatch(c, call).await } else { skill_handler::dispatch(c, call).await }
}
