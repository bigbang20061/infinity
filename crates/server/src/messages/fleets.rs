//! Fleet player-visible text used by `services/fleets/fleet_object_handler.rs`.
//! Fleet state is process-local.

// Fleets (services/fleets.rs)
/// Initial fleet wing display name.
pub(crate) const FLEET_INITIAL_WING_NAME: &str = "联队 1";
/// Initial fleet squad display name.
pub(crate) const FLEET_INITIAL_SQUAD_NAME: &str = "中队 1";
/// Current character already belongs to a fleet.
pub(crate) const PILOT_ALREADY_IN_FLEET_SELF: &str = "你已在舰队中。";
/// Target pilot is not in the fleet.
pub(crate) const PILOT_NOT_IN_FLEET_TARGET: &str = "该飞行员不在舰队中。";
/// Fleet lookup/operation missing.
pub(crate) const FLEET_NOT_FOUND: &str = "该舰队已不存在。";
/// Fleet MOTD permission.
pub(crate) const FLEET_LEADER_REQUIRED_FOR_MOTD: &str = "只有舰队指挥官才能设置每日消息。";
/// Invite target already belongs to a fleet.
pub(crate) const PILOT_ALREADY_IN_FLEET: &str = "该飞行员已在舰队中。";
/// Pilot is not in fleet.
pub(crate) const PILOT_NOT_IN_FLEET: &str = "你不在该舰队中。";
/// Fleet member capacity reached.
pub(crate) const FLEET_FULL: &str = "舰队已满。";
/// Fleet invite dialog header.
pub(crate) const FLEET_INVITATION_HEADER: &str = "舰队邀请";
/// Fleet invitation expired.
pub(crate) const FLEET_INVITATION_EXPIRED: &str = "该邀请已过期。";
/// Fleet member removal permission.
pub(crate) const FLEET_LEADER_REQUIRED_TO_REMOVE_MEMBER: &str = "只有舰队指挥官才能移除成员。";
/// Fleet disband permission.
pub(crate) const FLEET_LEADER_REQUIRED_TO_DISBAND: &str = "只有舰队指挥官才能解散舰队。";
/// Fleet member movement permission.
pub(crate) const FLEET_LEADER_REQUIRED_TO_MOVE_MEMBER: &str = "只有舰队指挥官才能调动成员。";
/// Fleet wing creation permission.
pub(crate) const FLEET_LEADER_REQUIRED_TO_ADD_WING: &str = "只有舰队指挥官才能添加联队。";
/// Fleet wing lookup missing.
pub(crate) const FLEET_WING_NOT_FOUND: &str = "该联队已不存在。";
/// Fleet squad creation permission.
pub(crate) const FLEET_LEADER_REQUIRED_TO_ADD_SQUAD: &str = "只有舰队指挥官才能添加中队。";
/// Fleet structure cannot be removed while occupied.
pub(crate) const FLEET_STRUCTURE_MUST_BE_EMPTY: &str = "请先将其中的成员移出。";
/// Fleet structure permission.
pub(crate) const FLEET_LEADER_REQUIRED_TO_CHANGE_STRUCTURE: &str = "只有舰队指挥官才能调整舰队结构。";
/// Fleet hierarchy rename permission.
pub(crate) const FLEET_LEADER_REQUIRED_TO_RENAME_WING_OR_SQUAD: &str = "只有舰队指挥官才能重命名联队和中队。";
/// Fleet invitation question.
pub(crate) fn fleet_invitation(name: &str) -> String { format!("{name}邀请你加入舰队。是否接受？") }
/// Default fleet wing name.
pub(crate) fn fleet_wing_name(n: usize) -> String { format!("联队 {n}") }
/// Default fleet squad name.
pub(crate) fn fleet_squad_name(n: usize) -> String { format!("中队 {n}") }
