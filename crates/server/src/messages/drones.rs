//! Player-visible drone command refusals.

pub(crate) const NOT_AVAILABLE: &str = "该无人机不可用。";
pub(crate) const INVALID_COMMAND: &str = "无人机指令无效。";
pub(crate) const NOT_CONTROLLED: &str = "你没有控制该无人机。";
pub(crate) const TARGET_OUT_OF_RANGE: &str = "目标超出无人机控制范围。";
pub(crate) const BANDWIDTH_EXCEEDED: &str = "无人机带宽不足。";
pub(crate) const MAX_ACTIVE_EXCEEDED: &str = "已达到无人机控制数量上限。";
pub(crate) const BAY_FULL: &str = "无人机舱容量不足。";
pub(crate) const DELEGATE_NOT_IN_FLEET: &str = "目标驾驶员不在你的舰队中。";
pub(crate) const NOT_IN_SPACE: &str = "必须在太空中才能执行无人机指令。";
pub(crate) const TRANSITION_BUSY: &str = "无人机正在执行另一项操作。";
