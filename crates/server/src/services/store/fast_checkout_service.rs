//! FastCheckoutService client fastcheckout/client answers; real-money checkout is disabled.
//! This fixed response service does not access data sources.
use super::*;

pub(super) async fn dispatch(_c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    Some(match (call.service, call.method) {
        // Fast checkout is the real-money PLEX purchase flow (fastcheckout/client): off. The China
        // funnel sends "buy PLEX" to the store instead of a card checkout.
        // GetTestingConfiguration -> (isChinaFunnel, fakeBuyPlexOfferUrl, useShellExecute)
        ("FastCheckoutService", "GetTestingConfiguration") => Ok(Value::tuple([Value::Bool(true), s(""), Value::Bool(false)])),
        ("FastCheckoutService", "IsFastCheckoutEnabledForUser") => Ok(Value::Bool(false)),
        ("FastCheckoutService", "ClearFastCheckoutCacheForUser") => Ok(Value::None),
        ("FastCheckoutService", "GetOffersForUser") => Ok(Value::dict([("plex", Value::list([]))])),
        ("FastCheckoutService", "BuyOffer") => Ok(Value::Bool(false)),

        _ => return None,
    })
}
