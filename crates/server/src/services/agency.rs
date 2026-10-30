//! Smaller agency and corporation services backed by SDE data or game tables:
//! epic arc journal messages, militia enlistment whitelists, station offices,
//! the roguelike lab's exploration state, and client bean (telemetry) delivery.

use super::{db_err, Call, CallResult};
use crate::client::Client;
use crate::consts::{FACTION_AMARR, FACTION_ANGEL_CARTEL, FACTION_CALDARI, FACTION_GALLENTE, FACTION_GURISTAS, FACTION_MINMATAR, SECONDS_PER_HOUR};
use marshal::Value;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, info};

/// facwarCommon._ALL_WARFARE_FACTIONS: Caldari, Minmatar, Amarr, Gallente, Guristas, Angels.
const FW_FACTIONS: [i64; 6] = [FACTION_CALDARI, FACTION_MINMATAR, FACTION_AMARR, FACTION_GALLENTE, FACTION_GURISTAS, FACTION_ANGEL_CARTEL];
/// The roguelike lab's weekly reset (Monday), at the daily downtime turnover.
const WEEKLY_RESET_HOUR_UTC: u64 = 3;
/// Unix time convention: seconds per day.
const DAY_SECONDS: u64 = 86_400;
/// Standard calendar unit: days per week.
const DAYS_PER_WEEK: u64 = 7;
/// Calendar offset: the Unix epoch was three days after Monday.
const UNIX_EPOCH_MONDAY_OFFSET_DAYS: u64 = 3;

fn is_npc_corp(corp_id: i64) -> bool {
    (1_000_000..2_000_000).contains(&corp_id)
}

/// {messageType: {missionID: messageID}} for every epic arc journal message
/// (evemissions.common.epic_arcs_data MESSAGE_*). The epic arc controller asks once per
/// mission node, so the answer is built once.
async fn epic_arc_messages(c: &Client) -> CallResult {
    static CACHE: Mutex<Option<Value>> = Mutex::new(None);
    if let Some(v) = CACHE.lock().unwrap().clone() {
        return Ok(v);
    }
    let rows = c
        .db
        .query(
            "SELECT k, m.key::int, (m.data->'messages'->>k)::bigint
             FROM sde_native.missions m, jsonb_object_keys(m.data->'messages') k
             WHERE k LIKE 'messages.epicMission.journalText.%'
             ORDER BY k",
            &[],
        )
        .await
        .map_err(db_err)?;
    let mut by_type: Vec<(String, Vec<(Value, Value)>)> = vec![];
    for r in rows {
        let kind: String = r.get(0);
        let entry = (Value::Int(r.get::<_, i32>(1) as i64), Value::Int(r.get(2)));
        match by_type.last_mut() {
            Some((k, list)) if *k == kind => list.push(entry),
            _ => by_type.push((kind, vec![entry])),
        }
    }
    let v = Value::Dict(by_type.into_iter().map(|(k, list)| (Value::str(k), Value::Dict(list))).collect());
    *CACHE.lock().unwrap() = Some(v.clone());
    Ok(v)
}

/// Factions whose militias the corporation's members may enlist in directly. NPC
/// corporations allow every militia; player corporations keep a CEO-set whitelist.
async fn allowed_factions(c: &Client, corp_id: i64) -> CallResult {
    if is_npc_corp(corp_id) {
        return Ok(Value::list(FW_FACTIONS.map(Value::Int)));
    }
    let rows = c
        .db
        .query("SELECT faction_id FROM game.corp_enlistment_factions WHERE corporation_id = $1 ORDER BY faction_id", &[&corp_id])
        .await
        .map_err(db_err)?;
    Ok(Value::list(rows.iter().map(|r| Value::Int(r.get(0)))))
}

/// SetMyCorpAllowedEnlistmentFactions(factionIDs) from the corporation window's whitelist.
async fn set_allowed_factions(c: &Client, call: &Call<'_>) -> CallResult {
    let Some(corp_id) = c.session.int("corpid").filter(|id| !is_npc_corp(*id)) else { return Ok(Value::None) };
    let factions: Vec<i64> = match call.args.first() {
        Some(Value::List(l) | Value::Tuple(l)) => l.iter().filter_map(|v| v.as_int()).filter(|f| FW_FACTIONS.contains(f)).collect(),
        _ => vec![],
    };
    c.db.execute("DELETE FROM game.corp_enlistment_factions WHERE corporation_id = $1", &[&corp_id]).await.map_err(db_err)?;
    for f in &factions {
        c.db
            .execute("INSERT INTO game.corp_enlistment_factions (corporation_id, faction_id) VALUES ($1, $2)", &[&corp_id, f])
            .await
            .map_err(db_err)?;
    }
    info!(user = c.login.user_id, "corp {corp_id}: militia whitelist {factions:?}");
    Ok(Value::None)
}

/// Unix seconds of the next Monday reset.
fn next_weekly_reset(now: u64) -> u64 {
    let today = now / DAY_SECONDS * DAY_SECONDS + WEEKLY_RESET_HOUR_UTC * SECONDS_PER_HOUR as u64;
    (0..=DAYS_PER_WEEK)
        .map(|d| today + d * DAY_SECONDS)
        .find(|t| *t > now && (t / DAY_SECONDS + UNIX_EPOCH_MONDAY_OFFSET_DAYS) % DAYS_PER_WEEK == 0)
        .unwrap_or(today + DAYS_PER_WEEK * DAY_SECONDS)
}

/// CallExploreRequested(): answered with OnExploreResponsed(text-format
/// eve.roguelike.exploration.Responsed). No weekly ships or runs are offered.
fn explore(c: &mut Client) -> CallResult {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let text = format!(
        "user {{ sequential: {} }}\nweekly_total_times: 0\nweekly_used_times: 0\nnext_refresh_time {{ seconds: {} }}\nmax_level: 0\n",
        c.login.user_id,
        next_weekly_reset(now)
    );
    c.notify("OnExploreResponsed", vec![Value::str(text)]);
    Ok(Value::None)
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let arg_int = |i: usize| call.args.get(i).and_then(|v| v.as_int());
    Some(match (call.service, call.method) {
        ("agentMgr", "GetMessagesForEpicArcMissions") => epic_arc_messages(c).await,
        ("fwCharacterEnlistmentMgr", "GetCorpAllowedEnlistmentFactions") => {
            let corp = arg_int(0).or(c.session.int("corpid")).unwrap_or(0);
            allowed_factions(c, corp).await
        }
        ("fwCharacterEnlistmentMgr", "SetMyCorpAllowedEnlistmentFactions") => set_allowed_factions(c, call).await,
        // Bound to the docked station; no corporation rents offices here.
        ("roguelikeMgr", "CallExploreRequested") => explore(c),
        // alert.__BeanDeliveryBoy: {counterID: (count, time)}, or the zlib-compressed group form.
        ("alert", "BeanDelivery" | "GroupBeanDelivery") => {
            let entries = match call.args.first() {
                Some(Value::Dict(d)) => d.len(),
                _ => 0,
            };
            debug!(user = c.login.user_id, "client {}: {entries} counters", call.method);
            Ok(Value::None)
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weekly_reset_is_next_monday() {
        // 2026-10-27 08:00 UTC is a Tuesday; the next reset is Monday 2026-11-02 03:00 UTC.
        assert_eq!(next_weekly_reset(1_793_088_000), 1_793_588_400);
    }
}
