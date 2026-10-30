//! Corporation militia whitelist methods for `fwCharacterEnlistmentMgr`.
//! Mirrors the corporation window enlistment calls and `facwarCommon` faction list.
//! Player corporation whitelists use `game.corp_enlistment_factions`; NPCs allow all factions.
//! This service does not model militia enlistment itself.

use super::super::{db_err, Call, CallResult};
use crate::client::Client;
use crate::consts::{FACTION_AMARR, FACTION_ANGEL_CARTEL, FACTION_CALDARI, FACTION_GALLENTE, FACTION_GURISTAS, FACTION_MINMATAR};
use crate::dao::corporations::enlistment;
use marshal::Value;
use tracing::info;

/// facwarCommon._ALL_WARFARE_FACTIONS: Caldari, Minmatar, Amarr, Gallente, Guristas, Angels.
const FW_FACTIONS: [i64; 6] = [FACTION_CALDARI, FACTION_MINMATAR, FACTION_AMARR, FACTION_GALLENTE, FACTION_GURISTAS, FACTION_ANGEL_CARTEL];

fn is_npc_corp(corp_id: i64) -> bool {
    (1_000_000..2_000_000).contains(&corp_id)
}

/// Factions whose militias the corporation's members may enlist in directly. NPC
/// corporations allow every militia; player corporations keep a CEO-set whitelist.
async fn allowed_factions(c: &Client, corp_id: i64) -> CallResult {
    if is_npc_corp(corp_id) {
        return Ok(Value::list(FW_FACTIONS.map(Value::Int)));
    }
    let rows = enlistment::allowed(&c.db, corp_id).await.map_err(db_err)?;
    Ok(Value::list(rows.into_iter().map(Value::Int)))
}

/// SetMyCorpAllowedEnlistmentFactions(factionIDs) from the corporation window's whitelist.
async fn set_allowed_factions(c: &Client, call: &Call<'_>) -> CallResult {
    let Some(corp_id) = c.session.int("corpid").filter(|id| !is_npc_corp(*id)) else {
        return Ok(Value::None);
    };
    let factions: Vec<i64> = match call.args.first() {
        Some(Value::List(l) | Value::Tuple(l)) => l.iter().filter_map(|v| v.as_int()).filter(|f| FW_FACTIONS.contains(f)).collect(),
        _ => vec![],
    };
    enlistment::clear(&c.db, corp_id).await.map_err(db_err)?;
    for f in &factions {
        enlistment::insert(&c.db, corp_id, *f).await.map_err(db_err)?;
    }
    info!(user = c.login.user_id, "corp {corp_id}: militia whitelist {factions:?}");
    Ok(Value::None)
}

pub(super) async fn dispatch(c: &Client, call: &Call<'_>) -> Option<CallResult> {
    let arg_int = |i: usize| call.args.get(i).and_then(|v| v.as_int());
    Some(match call.method {
        "GetCorpAllowedEnlistmentFactions" => {
            let corp = arg_int(0).or(c.session.int("corpid")).unwrap_or(0);
            allowed_factions(c, corp).await
        }
        "SetMyCorpAllowedEnlistmentFactions" => set_allowed_factions(c, call).await,
        _ => return None,
    })
}
