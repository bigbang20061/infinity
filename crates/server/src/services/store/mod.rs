//! New Eden Store, PLEX vault, redeeming queue and seasonal login rewards.
//!
//! * `storeManager` (vgs/client/vgsClient.py): the catalogue as the JSON-shaped
//!   dicts vgs/common/utils.py parses. The live catalogue was server-side only, so
//!   it is rebuilt from this build's data: Serenity-visible SKINs, apparel and
//!   crates that traded on the retail market, priced in PLEX at the retail
//!   ISK/PLEX ratio (crate::retail), plus skill services and a free daily gift.
//!   Category ids are the ones the store UI has icons for (nes_constants.py).
//! * `vaultManager`: the account's PLEX.
//! * `userSvc` redeem tokens (eve/client/script/ui/services/redeemsvc.py): what
//!   purchases and rewards deliver; claiming turns them into items, or free skill
//!   points for the skill-point types the client lists in redeem/data.py.
//! * `seasonalLoginCampaignManager` (loginrewards/client/seasonalLoginCampaignService.py):
//!   the newest seasonal campaign in seasonalCampaignsLoginRewards, one claim per day.

use crate::dao::items::loading::{load_items, ItemQuery};
use super::{db_err, keyval, Call, CallError, CallResult};
use crate::client::{filetime_now, unix_seconds_from_filetime, Client};
use crate::consts::FLAG_HANGAR;
use crate::messages;
use marshal::Value;
use std::sync::OnceLock;
use tracing::info;

/// Daily turnover of login rewards and daily offers (UTC; 11:00 Beijing time).
const TURNOVER: &str = "03:00:00";
/// Existing server policy: daily reward turnover offset from midnight UTC, in seconds.
const TURNOVER_OFFSET_SECONDS: i64 = 10_800;

fn s(v: &str) -> Value {
    Value::unicode(v)
}


/// `YYYY-MM-DDTHH:MM:SSZ` of the current daily period's start.
fn period_start() -> String {
    let secs = unix_seconds_from_filetime(filetime_now()) - TURNOVER_OFFSET_SECONDS;
    let day = secs.div_euclid(86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = day + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{TURNOVER}Z")
}


// ---------------------------------------------------------------- PLEX and redeeming

/// Queue `quantity` x `type_id` on `user_id`'s redeeming queue.
async fn grant_token(c: &Client, user_id: i64, type_id: i32, quantity: i64, label: &str) -> Result<(), CallError> {
    crate::dao::store::purchases::grant(&c.db, user_id, type_id, quantity as i32, label, filetime_now()).await.map_err(db_err)?;
    Ok(())
}


/// `typeID:quantity,typeID:quantity,...`
fn parse_rewards(s: &str) -> Vec<(i32, i64)> {
    s.split(',').filter_map(|p| {
        let (t, q) = p.split_once(':')?;
        Some((t.parse().ok()?, q.parse().ok()?))
    })
    .collect()
}


mod store_manager;
mod vault_manager;
mod user_svc;
mod fast_checkout_service;
mod seasonal_login_campaign_manager;
mod login_reward_facilities;

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.service {
        "storeManager" => store_manager::dispatch(c, call).await,
        "vaultManager" => vault_manager::dispatch(c, call).await,
        "userSvc" => user_svc::dispatch(c, call).await,
        "FastCheckoutService" => fast_checkout_service::dispatch(c, call).await,
        "seasonalLoginCampaignManager" => seasonal_login_campaign_manager::dispatch(c, call).await,
        "loginRewardFacilities" => login_reward_facilities::dispatch(c, call).await,
        _ => None,
    }
}

#[cfg(test)] mod tests;
