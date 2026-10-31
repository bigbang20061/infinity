//! XMPP chat routing and session channels, from server configuration and session state.
use super::*;
pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    Some(Ok(match call.method {
        "Hostname" => Value::str(&c.cfg.chat.domain),
        "GetDeprecatedPrefsFallback" => Value::str(&c.cfg.server.public_host),
        "ResyncSystemChannelAccess" => {
            let channels = [("local", "solarsystemid2"), ("corp", "corpid"), ("alliance", "allianceid"), ("faction", "warfactionid"), ("fleet", "fleetid")]
                .into_iter().filter_map(|(prefix, key)| c.session.int(key).map(|id| Value::str(format!("{prefix}_{id}"))));
            Value::list(channels)
        }
        _ => return None,
    }))
}
