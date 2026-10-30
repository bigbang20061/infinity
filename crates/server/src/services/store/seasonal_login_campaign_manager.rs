//! seasonalLoginCampaignManager for loginrewards/client/seasonalLoginCampaignService.py.
//! Uses SDE campaign data and game reward claims; only the newest campaign is supported.
use super::*;
/// Daily turnover of login rewards and daily offers (UTC; 11:00 Beijing time).
const TURNOVER: &str = "03:00:00";
const REWARD_DAY_SQL: &str = "(now() AT TIME ZONE 'UTC' - interval '3 hours')::date";
/// The newest seasonal campaign: (id, alpha rewards, omega rewards), each reward `(typeID, quantity)` by day.
async fn seasonal_campaign(c: &Client) -> Result<Option<(i32, Vec<(i32, i64)>, Vec<(i32, i64)>)>, CallError> {
    // Rewards as `typeID:quantity,...` in day order (jsonb {"0": {"typeID", "quantity"}, ...}).
    let track = |col: &str| {
        format!(
            "(SELECT string_agg((v->>'typeID') || ':' || COALESCE(v->>'quantity', '1'), ',' ORDER BY k::int)
              FROM jsonb_each(data->'{col}') AS e(k, v))"
        )
    };
    let row = c
        .db
        .query_opt(
            &format!(
                "SELECT key::int AS campaign_id, {} AS alpha_rewards, {} AS omega_rewards FROM sde_native.seasonalcampaignsloginrewards
                 WHERE data ? 'alphaRewards' AND data ? 'omegaRewards' ORDER BY key::int DESC LIMIT 1",
                track("alphaRewards"),
                track("omegaRewards")
            ),
            &[],
        )
        .await
        .map_err(db_err)?;
    let Some(row) = row else { return Ok(None) };
    let rewards = |name: &str| parse_rewards(&row.get::<_, Option<String>>(name).unwrap_or_default());
    Ok(Some((row.get("campaign_id"), rewards("alpha_rewards"), rewards("omega_rewards"))))
}
/// (claims so far, claimed today) for `campaign`.
async fn seasonal_claims(c: &Client, campaign: i32) -> Result<(i64, bool), CallError> {
    crate::dao::store::rewards::claims(&c.db, c.login.user_id, campaign).await.map_err(db_err)
}

async fn seasonal(c: &mut Client, call: &Call<'_>) -> CallResult {
    let Some((campaign, alpha, omega)) = seasonal_campaign(c).await? else {
        return Ok(match call.method {
            "get_active_campaign" => Value::tuple([Value::None, Value::None, Value::None, Value::None]),
            "get_claim_history" => Value::Dict(vec![]),
            _ => Value::Bool(false),
        });
    };
    let total = alpha.len().min(omega.len()) as i64;
    let (claimed, today) = seasonal_claims(c, campaign).await?;
    match call.method {
        // (campaignID, end date ordinal, daily turnover, rewards remaining). The campaign stays
        // open until every reward is claimed: it ends the day after the last one would be.
        "get_active_campaign" => {
            let remaining = total - claimed;
            let today_ordinal: i32 =
                c.db.query_one(&format!("SELECT ({REWARD_DAY_SQL} - date '0001-01-01') + 1 AS ordinal"), &[]).await.map_err(db_err)?.get("ordinal");
            let end = today_ordinal as i64 + remaining + 1;
            Ok(Value::tuple([Value::Int(campaign as i64), Value::Int(end), Value::str(TURNOVER), Value::Int(remaining)]))
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
            for (type_id, quantity) in [alpha[day], omega[day]] {
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
