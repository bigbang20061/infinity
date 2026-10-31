//! Player-visible bounty and kill-right refusals.
//! These messages do not replace the client's named kill-right error catalogue.

pub(crate) const UNKNOWN_OWNER: &str = "无法找到该悬赏目标。";
pub(crate) const INVALID_BOUNTY_AMOUNT: &str = "悬赏金额超出允许范围。";
pub(crate) const NO_BOUNTY: &str = "该目标没有悬赏。";
pub(crate) const NOT_RIGHT_OWNER: &str = "只有击杀权拥有者可以执行此操作。";
pub(crate) const INVALID_RESTRICTION: &str = "无法找到指定的击杀权购买对象。";
pub(crate) const INVALID_PRICE: &str = "击杀权价格无效。";
