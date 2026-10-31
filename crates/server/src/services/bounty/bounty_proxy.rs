//! bountyProxy client methods, mirroring eve/client/script/ui/shared/bountySvc.py.
//! Persistence and transactions live in the bounty mechanics modules.
use super::*;
use crate::services::value_ids;

pub(super) async fn dispatch(c: &Client, call: &Call<'_>) -> Option<CallResult> {
    let int = |i: usize| call.args.get(i).and_then(|v| v.as_int()).unwrap_or(0);
    let number = |i: usize| call.args.get(i).and_then(|v| v.as_float().or_else(|| v.as_int().map(|n| n as f64))).unwrap_or(f64::NAN);
    Some(match call.method {
        "AddToBounty" => bounties::add(c, int(0), number(1)).await,
        "GetBounties" => bounties::get(c, &value_ids(call.args.first())).await,
        "GetBountiesAndKillRights" => bounties::combined(c, &value_ids(call.args.first()), &value_ids(call.args.get(1))).await,
        "GetMyBounties" => bounties::mine(c).await,
        "GetKillRightsOnCharacters" => kill_rights::visible(c, &value_ids(call.args.first())).await,
        "GetMyKillRights" => kill_rights::mine(c).await,
        "SellKillRight" => kill_rights::sell(c, int(0), number(1), call.kwargs.get("restrictedTo").or_else(|| call.args.get(2)).and_then(|v| v.as_int())).await,
        "CancelSellKillRight" => kill_rights::cancel(c, int(0), int(1)).await,
        "GetTopPilotBounties" => bounties::top(c, bounties::CHARACTER, int(0)).await,
        "GetTopCorpBounties" => bounties::top(c, bounties::CORPORATION, int(0)).await,
        "GetTopAllianceBounties" => bounties::top(c, bounties::ALLIANCE, int(0)).await,
        "GetTopPilotBountyHunters" => bounties::hunters(c, bounties::CHARACTER, int(0)).await,
        "GetTopCorporationBountyHunters" => bounties::hunters(c, bounties::CORPORATION, int(0)).await,
        "GetTopAllianceBountyHunters" => bounties::hunters(c, bounties::ALLIANCE, int(0)).await,
        "SearchCharBounties" => bounties::search(c, bounties::CHARACTER, int(0)).await,
        "SearchCorpBounties" => bounties::search(c, bounties::CORPORATION, int(0)).await,
        "SearchAllianceBounties" => bounties::search(c, bounties::ALLIANCE, int(0)).await,
        _ => return None,
    })
}
