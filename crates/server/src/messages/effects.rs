//! Server-authored dogma effect and warp refusal text.
//! The client displays this text through `eveexceptions.UserError` using the
//! `CustomNotify` catalogue key. Effect names come from SDE data and activation
//! requests; no database is read here. This module does not implement effects.

/// Refusal for an effect that the server does not implement.
pub(crate) fn effect_not_supported(effect: &str) -> String { format!("不支持效果：{effect}。") }

/// Warp refusal corresponding to the autopilot.py warp-scrambled message.
pub(crate) const WARP_SCRAMBLED: &str = "你的舰船受到跃迁扰断，无法跃迁。";

/// Cloaking refusals displayed through CustomNotify.
pub(crate) const CLOAK_TOO_CLOSE: &str = "距离附近物体太近，无法启动隐形装置。";
pub(crate) const CLOAK_REACTIVATION_DELAY: &str = "隐形装置尚未完成重新激活冷却。";
pub(crate) const CLOAK_TARGETING_DELAY: &str = "解除隐形后的锁定延迟尚未结束。";
pub(crate) const CLOAK_UNSUITABLE: &str = "当前舰船状态不允许启动隐形装置。";
pub(crate) const CLOAK_ALREADY_ACTIVE: &str = "隐形装置已经启动。";
/// Micro jump drive refusal displayed through CustomNotify.
pub(crate) const MICRO_JUMP_BLOCKED: &str = "舰船受到跃迁干扰，无法启动微型跳跃引擎。";
