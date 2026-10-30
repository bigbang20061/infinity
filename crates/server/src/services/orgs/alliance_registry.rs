//! Bound alliance bulletin, contact and label `allianceRegistry` calls, mirroring
//! `eve/client/script/ui/services/alliances`.
//! `services::orgs` owns the shared implementation, backed by
//! `game.org_bulletins`, `game.org_contacts` and `game.org_labels`.
//! Unbound alliance calls and other alliance registry operations are outside this handler.
use super::{Call, CallResult, Client};
pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service != "allianceRegistry" || !call.bound { return None; }
    super::dispatch_shared(c, call).await
}
