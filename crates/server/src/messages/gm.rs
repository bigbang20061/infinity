//! GM slash-command and per-item dogma override messages.
//! The client slash service consumes these strings; they read no database and
//! implement neither gameplay behavior nor localisation.

// GM slash commands (services/slash.rs)
/// Teleport command help.
pub(crate) const GM_HELP_TELEPORT: &str = "/tr me <itemID|characterID|solarSystemID|stationID|celestialID> — teleport";
/// Wormhole command help.
pub(crate) const GM_HELP_WORMHOLE: &str = "/wh — teleport next to a wormhole (scanned down for you)";
/// Scan-all command help.
pub(crate) const GM_HELP_SCAN_ALL: &str = "/scanall — scan every signature in this system to 100%";
/// Site listing command help.
pub(crate) const GM_HELP_SITES: &str = "/sites — list this system's anomalies and signatures";
/// Create command help.
pub(crate) const GM_HELP_CREATE: &str = "/create <typeID|name> [qty] — into your hangar (docked) or cargo";
/// Create-item command help.
pub(crate) const GM_HELP_CREATE_ITEM: &str = "/createitem <typeID|name> [qty] — same as /create";
/// Set ISK command help.
pub(crate) const GM_HELP_SET_ISK: &str = "/setisk <amount> — set your wallet";
/// Add ISK command help.
pub(crate) const GM_HELP_ADD_ISK: &str = "/addisk <amount> — add ISK to your wallet";
/// Set PLEX command help.
pub(crate) const GM_HELP_SET_PLEX: &str = "/setplex <count> — set your PLEX vault";
/// Add PLEX command help.
pub(crate) const GM_HELP_ADD_PLEX: &str = "/addplex <count> — add PLEX to your vault";
/// Heal command help.
pub(crate) const GM_HELP_HEAL: &str = "/heal — repair your ship in space";
/// Help command help.
pub(crate) const GM_HELP_HELP: &str = "/help — this list";
/// Teleport command usage error.
pub(crate) const GM_TELEPORT_USAGE: &str = "usage: /tr me <itemID|characterID|solarSystemID>";
/// Create command usage error.
pub(crate) const GM_CREATE_USAGE: &str = "usage: /create <typeID|name> [qty]";
/// Set ISK command usage error.
pub(crate) const GM_SET_ISK_USAGE: &str = "usage: /setisk <amount>";
/// Add ISK command usage error.
pub(crate) const GM_ADD_ISK_USAGE: &str = "usage: /addisk <amount>";
/// Set PLEX command usage error.
pub(crate) const GM_SET_PLEX_USAGE: &str = "usage: /setplex <count>";
/// Add PLEX command usage error.
pub(crate) const GM_ADD_PLEX_USAGE: &str = "usage: /addplex <count>";
/// Wallet numeric overflow.
pub(crate) const GM_AMOUNT_TOO_LARGE: &str = "amount exceeds the wallet limit";
/// No destination for a created item.
pub(crate) const GM_NOWHERE_TO_PUT_ITEM: &str = "nowhere to put it";
/// Docked ships repair automatically.
pub(crate) const GM_DOCKED_SHIP_REPAIRS: &str = "docked ships repair themselves";
/// Ship repair result.
pub(crate) const GM_REPAIRED: &str = "repaired";
/// Missing GM role.
pub(crate) const GM_ROLE_REQUIRED: &str = "you are not a GM";
/// Unknown item type.
pub(crate) fn gm_unknown_type(arg: &str) -> String { format!("no such type: {arg}") }
/// Invalid destination ID.
pub(crate) fn gm_invalid_id(dest: &str) -> String { format!("not an ID: {dest}") }
/// Teleport result.
pub(crate) fn gm_teleported(id: i64) -> String { format!("teleported to {id}") }
/// Unknown solar system.
pub(crate) fn gm_unknown_system(id: i64) -> String { format!("unknown solar system {id}") }
/// Solar-system teleport result.
pub(crate) fn gm_teleported_to_system(id: i64) -> String { format!("teleported to system {id}") }
/// Teleport to an item in another system.
pub(crate) fn gm_teleported_in_system(id: i64, system: i64) -> String { format!("teleported to {id} in {system}") }
/// Unknown teleport target.
pub(crate) fn gm_target_not_found(id: i64) -> String { format!("can't find {id}") }
/// Created item result.
pub(crate) fn gm_created_item(qty: i32, name: &str, type_id: i32) -> String { format!("created {qty} x {name} ({type_id})") }
/// Updated ISK balance, with a client projection when clamped.
pub(crate) fn gm_balance(exact: &str, shown: Option<f64>) -> String {
    let mut result = format!("balance {exact} ISK");
    if let Some(shown) = shown { result.push_str(&format!(" (client shows {shown:.0})")); }
    result
}
/// Updated PLEX vault, with a client projection when clamped.
pub(crate) fn gm_plex_balance(exact: &str, shown: Option<i64>) -> String {
    let mut result = format!("vault {exact} PLEX");
    if let Some(shown) = shown { result.push_str(&format!(" (client shows {shown})")); }
    result
}
/// One quoted command in the command list.
pub(crate) fn gm_command_name(n: &str) -> String { format!("'{n}'") }
/// Remote command list.
pub(crate) fn gm_command_list(names: &str) -> String { format!("commands: [{}]", names) }
/// Unknown command and help text.
pub(crate) fn gm_unknown_command(other: &str, help: &str) -> String { format!("unknown command /{other}\n{}", help) }

// GM per-item dogma overrides.
pub(crate) const GM_HELP_DOGMA: &str = "/dogma <ship|物品ID|flag:N> <属性ID|名称> [数值|reset]；/dogma <目标> list";
pub(crate) const GM_DOGMA_USAGE: &str = "用法：/dogma <ship|物品ID|flag:N> <属性ID|名称> [数值|reset]，或 /dogma <目标> list。";
pub(crate) const GM_DOGMA_NO_SHIP: &str = "没有当前飞船。";
pub(crate) const GM_DOGMA_SINGLETON: &str = "只能修改单件物品，请使用已组装的单件物品。";
pub(crate) const GM_DOGMA_FORBIDDEN_TARGET: &str = "不能修改弹药、角色、建筑或 NPC 的属性。";
pub(crate) const GM_DOGMA_RUNTIME: &str = "此属性由运行状态管理；护盾、装甲和结构请使用 /heal，在线状态请使用模块在线开关。";
pub(crate) const GM_DOGMA_CHARACTER: &str = "角色属性不能通过 /dogma 修改。";
pub(crate) const GM_DOGMA_ACTIVE_CYCLE: &str = "运行中的模块不能修改周期属性；请先关闭模块。";
pub(crate) const GM_DOGMA_VISIBILITY: &str = "模块或船体原本没有的属性不会显示在装配窗口和 HUD 最大值中。";
pub(crate) const GM_DOGMA_BASE_ONLY: &str = "目标不在当前装配中；仅显示基础值，实际有效值取决于其装配和驾驶员。";
pub(crate) const GM_DOGMA_EMPTY: &str = "没有属性覆盖。";
pub(crate) fn gm_dogma_invalid_target(s: &str) -> String { format!("无效的目标：{s}") }
pub(crate) fn gm_dogma_attribute(s: &str) -> String { format!("未知属性：{s}") }
pub(crate) fn gm_dogma_ambiguous(s: &str, ids: &str) -> String { format!("属性名称 {s} 不唯一，请使用 ID：{ids}") }
pub(crate) fn gm_dogma_result(item: i64, attr: &str, id: i32, old_base: f64, new_base: f64, effective: Option<(f64, f64)>, overridden: bool) -> String {
    let effective = effective.map(|(old, new)| format!("；有效值 {old} → {new}")).unwrap_or_default();
    format!("物品 {item} 属性 {attr} ({id})：基础值 {old_base} → {new_base}{effective}；覆盖：{}", if overridden { "是" } else { "否" })
}
pub(crate) fn gm_dogma_noop(item: i64, id: i32) -> String { format!("物品 {item} 的属性 {id} 没有覆盖，无需重置。") }
pub(crate) fn gm_dogma_list_item(flag: i32, name: &str, item: i64) -> String { format!("位置 {flag}，{name}，物品 {item}") }
pub(crate) fn gm_dogma_list_entry(id: i32, value: f64) -> String { format!("  属性 {id}: {value}") }
