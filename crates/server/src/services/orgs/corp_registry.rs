//! Corporation bulletin, contact and label `corpRegistry` calls, mirroring
//! `eve/client/script/ui/services/corporation`.
//! `services::orgs` owns the shared implementation, backed by
//! `game.org_bulletins`, `game.org_contacts` and `game.org_labels`.
//! Other corporation registry operations are outside this handler.
use super::{Call, CallResult, Client};
pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service != "corpRegistry" { return None; }
    super::dispatch_shared(c, call).await
}
