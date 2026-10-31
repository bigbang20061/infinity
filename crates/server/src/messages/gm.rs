//! GM slash-command and per-item dogma override messages.
//! The client slash service consumes these strings; they read no database and
//! implement neither gameplay behavior nor localisation.

/// An argument quote was not closed.
pub(crate) const GM_ARGS_UNCLOSED_QUOTE: &str = "引号未闭合。";
/// A command requires a selected character.
pub(crate) const GM_ARGS_NO_CHARACTER: &str = "当前没有选择角色。";
pub(crate) fn gm_args_missing(index: usize) -> String { format!("缺少第 {index} 个参数。") }
pub(crate) fn gm_args_integer(text: &str) -> String { format!("无效的整数：{text}") }
pub(crate) fn gm_args_number(text: &str) -> String { format!("无效的数值：{text}") }
pub(crate) fn gm_args_unknown_target(text: &str) -> String { format!("找不到角色：{text}") }
pub(crate) fn gm_rows_truncated(count: usize) -> String { format!("另有 {count} 行未显示。") }

// GM slash commands (services/slash.rs)
/// Teleport command help.
pub(crate) const GM_HELP_TELEPORT: &str = "/tr me <itemID|characterID|solarSystemID|stationID|celestialID> — teleport";
/// Wormhole command help.
pub(crate) const GM_HELP_WORMHOLE: &str = "/wh — teleport next to a wormhole (scanned down for you)";
/// Scan-all command help.
pub(crate) const GM_HELP_SCAN_ALL: &str = "/scanall — scan every signature in this system to 100%";
/// Site listing command help.
pub(crate) const GM_HELP_SITES: &str = "/sites — 列出当前星系的异常和信号强度";
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

/// Effect diagnostics command help and usage.
pub(crate) const GM_HELP_EFFECTS: &str = "/effects unsupported — 列出未支持的效果";
/// Invalid effect diagnostics arguments.
pub(crate) const GM_EFFECTS_USAGE: &str = "用法：/effects unsupported";
/// No unsupported effects have been observed.
pub(crate) const GM_EFFECTS_EMPTY: &str = "尚未记录未支持的效果。";
/// Format the observed unsupported effects in snapshot order.
pub(crate) fn gm_effects_unsupported(effects: &[(String, i32)]) -> String {
    if effects.is_empty() {
        return GM_EFFECTS_EMPTY.into();
    }
    effects.iter().map(|(name, type_id)| format!("{name}（类型 ID：{type_id}）")).collect::<Vec<_>>().join("\n")
}

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

// Lua administration.
pub(crate) const LUA_HELP: &str = "用法：/lua reload 或 /lua list";
pub(crate) const LUA_RELOADED: &str = "脚本已重新加载。";
pub(crate) fn lua_reload_failed(error: &str) -> String { format!("脚本重新加载失败：{error}") }
pub(crate) fn lua_list(list: &str) -> String { format!("Lua 脚本：\n{list}") }

// Skill administration.
pub(crate) const GM_HELP_GIVESKILL: &str = "/giveskill <角色> <技能|all|super> [等级] — 设置技能等级";
pub(crate) const GM_HELP_ALLSKILLS: &str = "/allskills [角色] — 设置全部已发布技能";
pub(crate) const GM_HELP_REMOVESKILL: &str = "/removeskill <角色> <技能|all> — 移除技能";
pub(crate) const GM_HELP_GIVESTARTERSKILLS: &str = "/givestarterskills <角色> — 补充初始技能";
pub(crate) const GM_SKILL_LEVEL_INVALID: &str = "技能等级必须为 0 至 5。";
pub(crate) fn gm_skill_usage(command: &str) -> String {
    match command {
        "giveskill" => "用法：/giveskill <角色> <技能|all|super> [等级]",
        "allskills" => "用法：/allskills [角色]",
        "removeskill" => "用法：/removeskill <角色> <技能|all>",
        "givestarterskills" => "用法：/givestarterskills <角色>",
        _ => "无效的技能指令。",
    }.into()
}
pub(crate) fn gm_skill_not_skill(skill: &str) -> String { format!("{skill} 不是技能。") }
pub(crate) fn gm_skill_done(command: &str, target: i64) -> String { format!("{command} 已应用于角色 {target}。") }


// Item administration.
pub(crate) const GM_HELP_SHIP: &str = "/ship <类型> [角色] — 创建飞船";
pub(crate) const GM_HELP_SPAWN: &str = "/spawn <类型> — 停靠时创建飞船并返回物品ID";
pub(crate) const GM_HELP_FIT: &str = "/fit <角色> <类型或物品ID> [数量] — 装配模块";
pub(crate) const GM_HELP_UNLOAD: &str = "/unload me <物品ID或类型ID> — 卸下模块";
pub(crate) const GM_HELP_LOAD: &str = "/load me <类型ID> <数量> — 装填弹药";
pub(crate) const GM_HELP_ONLINE: &str = "/online <飞船ID|me> — 上线模块";
pub(crate) const GM_HELP_ITEMINFO: &str = "/iteminfo <物品ID> — 查看物品";
pub(crate) const GM_HELP_TYPEINFO: &str = "/typeinfo <类型> — 查看类型";
pub(crate) const GM_SHIP_TYPE_REQUIRED: &str = "指定类型不是飞船";
pub(crate) const GM_SHIP_DOCKED_REQUIRED: &str = "目标角色必须停靠在空间站";
pub(crate) const GM_ITEM_NOT_FOUND: &str = "找不到指定物品";
pub(crate) const GM_ITEM_USAGE: &str = "物品命令参数无效";
pub(crate) const GM_ITEM_TARGET_SELF: &str = "此命令仅支持当前角色";
pub(crate) const GM_ITEM_SHIP_USAGE: &str = "用法：/ship <类型> [目标]";
pub(crate) const GM_ITEM_SPAWN_USAGE: &str = "用法：/spawn <类型>";
pub(crate) const GM_ITEM_FIT_USAGE: &str = "用法：/fit <目标> <类型或物品ID> [数量]";
pub(crate) const GM_ITEM_UNLOAD_USAGE: &str = "用法：/unload me <物品ID或类型ID>";
pub(crate) const GM_ITEM_LOAD_USAGE: &str = "用法：/load me <类型ID> <数量>";
pub(crate) const GM_ITEM_ONLINE_USAGE: &str = "用法：/online <飞船ID|me>";
pub(crate) const GM_ITEM_INFO_USAGE: &str = "用法：/iteminfo <物品ID>";
pub(crate) const GM_TYPE_INFO_USAGE: &str = "用法：/typeinfo <类型>";
pub(crate) const GM_FIT_REFUSED: &str = "无法装配：目标、物品或可用槽位无效。";
pub(crate) fn gm_item_created(id: i64, name: &str) -> String { format!("已创建飞船 {name}（{id}）") }
pub(crate) fn gm_item_fitted(count: usize) -> String { format!("已装配 {count} 个模块") }
pub(crate) fn gm_item_unloaded(count: usize) -> String { format!("已卸下 {count} 个模块") }
pub(crate) fn gm_item_loaded(count: usize) -> String { format!("已向 {count} 个兼容模块提交装填请求") }
pub(crate) fn gm_item_online(count: usize) -> String { format!("已上线 {count} 个模块") }
pub(crate) fn gm_item_info(item_id: i64, type_id: i32, owner: i64, location: i64, flag: i32, quantity: i32, singleton: bool) -> String { format!("物品 {item_id}：类型 {type_id}，所有者 {owner}，位置 {location}，标志 {flag}，数量 {quantity}，单例 {singleton}") }
pub(crate) fn gm_type_info(type_id: i32, name: &str, group: i32, category: i32) -> String { format!("类型 {type_id}：{name}，组 {group}，分类 {category}") }
pub(crate) fn gm_type_attribute(id: i32, value: f64) -> String { format!("属性 {id}：{value}") }

// NPC, debris and damage administration.
pub(crate) const GM_HELP_ENTITY: &str = "/entity deploy <数量> <typeID> — 生成 NPC";
pub(crate) const GM_HELP_NPC: &str = "/npc <类型或组> [数量] [距离] — 生成 NPC";
pub(crate) const GM_HELP_NPCCLEAR: &str = "/npcclear [半径|system] — 清除 GM 生成的 NPC";
pub(crate) const GM_HELP_WRECK: &str = "/wreck [类型] [距离] — 生成残骸";
pub(crate) const GM_HELP_CONTAINER: &str = "/container [类型] [距离] — 生成货柜";
pub(crate) const GM_HELP_DMG: &str = "/dmg <shield|armor|hull|all> <百分比> — 设置舰船损伤";
pub(crate) const GM_NPC_USAGE: &str = "用法：/npc <类型或组> [数量] [距离]；/entity deploy <数量> <typeID>";
pub(crate) const GM_NPC_CLEAR_USAGE: &str = "用法：/npcclear [半径|system]";
pub(crate) const GM_DEBRIS_USAGE: &str = "用法：/wreck 或 /container [类型] [距离]";
pub(crate) const GM_DAMAGE_USAGE: &str = "用法：/dmg <shield|armor|hull|all> <0至100的百分比>";
pub(crate) const GM_DAMAGE_SET: &str = "舰船损伤已设置。";
pub(crate) fn gm_npc_spawned(count: usize) -> String { format!("已生成 {count} 个NPC。") }
pub(crate) fn gm_npc_cleared(count: usize) -> String { format!("已移除 {count} 个GM生成的NPC。") }
pub(crate) fn gm_debris_spawned(id: i64) -> String { format!("已生成太空物体 {id}。") }
