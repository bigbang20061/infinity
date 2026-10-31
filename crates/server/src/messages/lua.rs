//! Player-facing messages for the bundled Lua MOTD script. The client receives
//! these via slash responses and CustomNotify; this module reads no data itself.

pub(crate) const LUA_MOTD_HELP: &str = "用法：/motd 或 /motd set <公告>";
pub(crate) const LUA_MOTD_SAVED: &str = "公告已保存。";
pub(crate) const LUA_MOTD_EMPTY: &str = "暂无公告。";
