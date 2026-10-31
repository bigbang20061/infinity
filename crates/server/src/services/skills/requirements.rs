//! SDE skill prerequisites used by skill queue validation.
//! Mirrors dogma/const.py requiredSkill attributes; reads startup SDE type data.
//! Entitlement and recursive prerequisite policy are caller concerns.
use crate::{consts::{ATTR_REQUIRED_SKILLS, ATTR_REQUIRED_SKILL_LEVELS}, sde::Sde};

/// Required type and level pairs for a type, in SDE attribute slot order.
pub(crate) fn requirements(sde: &Sde, type_id: i32) -> impl Iterator<Item = (i32, i32)> + '_ {
    ATTR_REQUIRED_SKILLS.into_iter().zip(ATTR_REQUIRED_SKILL_LEVELS).filter_map(move |(skill, level)| {
        let skill = sde.types.attribute(type_id, skill)? as i32;
        let level = sde.types.attribute(type_id, level)? as i32;
        (skill > 0 && level > 0).then_some((skill, level))
    })
}
