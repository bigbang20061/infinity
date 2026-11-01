//! Player-visible refusals for mutual wars; aggressive wars, allies and surrender are unsupported.
pub(crate) const INVALID: &str = "战争请求无效或已失效。";
pub(crate) const AUTHORITY: &str = "你没有权限管理此组织的战争。";
pub(crate) const AGGRESSIVE_UNAVAILABLE: &str = "目前只支持双方同意的战争。";
pub(crate) const ALLIES_UNAVAILABLE: &str = "双方同意的战争不支持盟友。";
pub(crate) const SURRENDER_UNAVAILABLE: &str = "双方同意的战争不支持投降协议。";
