//! Server-authored dogma effect and warp refusal text.
//! The client displays this text through `eveexceptions.UserError` using the
//! `CustomNotify` catalogue key. Effect names come from SDE data and activation
//! requests; no database is read here. This module does not implement effects.

/// Refusal for an effect that the server does not implement.
pub(crate) fn effect_not_supported(effect: &str) -> String { format!("不支持效果：{effect}。") }

/// Warp refusal corresponding to the autopilot.py warp-scrambled message.
pub(crate) const WARP_SCRAMBLED: &str = "你的舰船受到跃迁扰断，无法跃迁。";
