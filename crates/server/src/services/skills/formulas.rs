//! Skill point formulas from characterskills/util.py.
//! Uses a supplied skill rank, normally from sde.type_attributes; no tables are read here.
//! Levels above the client maximum are capped.
/// Client characterskills/util.py skill point formula base.
const SKILL_POINT_BASE: f64 = 250.0;
/// Client characterskills/util.py skill point formula exponential base.
const SKILL_LEVEL_BASE: f64 = 2.0;
/// Client characterskills/util.py skill point formula exponent multiplier.
const SKILL_LEVEL_EXPONENT: f64 = 2.5;
/// Client characterskills/util.py maximum trained skill level.
const MAX_SKILL_LEVEL: i64 = 5;

pub(super) fn sp_for_level(rank: f64, level: i64) -> i64 {
    if level <= 0 {
        return 0;
    }
    (SKILL_POINT_BASE * rank * SKILL_LEVEL_BASE.powf(SKILL_LEVEL_EXPONENT * (level.min(MAX_SKILL_LEVEL) - 1) as f64)).ceil() as i64
}
