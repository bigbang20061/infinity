//! Player refusals and client error keys for access-group operations.
/// Group is missing.
pub(crate) const MISSING: &str = "访问组不存在。";
/// Caller lacks the required access.
pub(crate) const DENIED: &str = "你没有权限执行此操作。";
/// Invalid group arguments.
pub(crate) const INVALID: &str = "访问组参数无效。";
/// Corporation-owned groups have no supported lifecycle.
pub(crate) const CORP_OWNED: &str = "暂不支持修改军团拥有的访问组。";
// ownergroups/ownergroupsUIUtil.py failure keys.
pub(crate) const ILLEGAL_MEMBER: &str = "IllegalMemberID";
pub(crate) const MANAGER_ADD: &str = "ManagersCanNotAddAdminsOrManagers";
pub(crate) const CHARACTER_REQUIRED: &str = "AdminsAndManagersMustBeCharacters";
pub(crate) const MANAGER_UPDATE: &str = "NotAllowedToUpdateAdminOrManager";
pub(crate) const PROMOTION: &str = "NotAllowedToPromoteToAdmin";
pub(crate) const LAST_ADMIN: &str = "IsTheLastAdmin";
