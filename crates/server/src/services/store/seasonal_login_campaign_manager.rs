//! seasonalLoginCampaignManager for loginrewards/client/seasonalLoginCampaignService.py.
//! Uses SDE campaign data and game reward claims; only the newest campaign is supported.
use super::*;
/// The newest seasonal campaign: (id, alpha rewards, omega rewards), each reward `(typeID, quantity)` by day.
fn seasonal_campaign(c: &Client) -> Option<&crate::sde::store::Campaign> { c.sde.store.campaign() }
/// (claims so far, claimed today) for `campaign`.
async fn seasonal_claims(c: &Client, campaign: i32) -> Result<(i64, bool), CallError> {
    crate::dao::store::rewards::claims(&c.db, c.login.user_id, campaign).await.map_err(db_err)
}

async fn seasonal(c: &mut Client, call: &Call<'_>) -> CallResult {
    let Some(data) = seasonal_campaign(c) else {
        return Ok(match call.method {
            "get_active_campaign" => Value::tuple([Value::None, Value::None, Value::None, Value::None]),
            "get_claim_history" => Value::Dict(vec![]),
            _ => Value::Bool(false),
        });
    };
    let campaign = data.id;
    let total = data.alpha.len().min(data.omega.len()) as i64;
    let (claimed, today) = seasonal_claims(c, campaign).await?;
    match call.method {
        // (campaignID, end date ordinal, daily turnover, rewards remaining). The campaign stays
        // open until every reward is claimed: it ends the day after the last one would be.
        "get_active_campaign" => {
            let remaining = total - claimed;
            let today_ordinal = crate::dao::store::rewards::today_ordinal(&c.db).await.map_err(db_err)?;
            let end = today_ordinal as i64 + remaining + 1;
            Ok(Value::tuple([Value::Int(campaign as i64), Value::Int(end), Value::str(&crate::config::game().store.daily_turnover_utc), Value::Int(remaining)]))
        }
        // {date ordinal: clone grade}
        "get_claim_history" => {
            let rows = crate::dao::store::rewards::history(&c.db, c.login.user_id, campaign).await.map_err(db_err)?;
            Ok(Value::Dict(rows.iter().map(|r| (Value::Int(r.ordinal as i64), Value::Int(r.clone_grade as i64))).collect()))
        }
        // Omega clones get both tracks' reward for the day.
        "claim_reward" => {
            if today || claimed >= total {
                return Ok(Value::Bool(false));
            }
            crate::dao::store::rewards::claim(&c.db, c.login.user_id, campaign).await.map_err(db_err)?;
            let day = claimed as usize;
            let data = c.sde.store.campaign().expect("campaign was present");
            let rewards = [data.alpha[day], data.omega[day]];
            for (type_id, quantity) in rewards {
                grant_token(c, c.login.user_id, type_id, quantity, "UI/LoginRewards/WindowCaption").await?;
            }
            info!(user = c.login.user_id, "login reward: campaign {campaign} day {} claimed", day + 1);
            c.notify("OnRedeemingQueueUpdated", vec![]);
            Ok(Value::Bool(true))
        }
        _ => Ok(Value::None),
    }
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    Some(match (call.service, call.method) {
        ("seasonalLoginCampaignManager", _) => seasonal(c, call).await,

        _ => return None,
    })
}

