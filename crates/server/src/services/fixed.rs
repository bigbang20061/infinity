//! Constant replies for unmodelled remote services.
//! Mirrors client remote-service calls; no database or state is accessed.
//! Stateful and argument-dependent services are handled by their feature modules.

use super::{notify_error, Call, CallResult};
use marshal::Value;

/// Fixed replies at the calendar/bookmark position in the dispatch chain.
pub(super) fn social_early(call: &Call<'_>) -> Option<CallResult> {
    Some(match (call.service, call.method) {
        ("notificationMgr", "LogNotificationInteraction") | ("accessGroupBookmarkMgr", "RemoveFromKnownFolders") => Ok(Value::None),
        ("mailingListsMgr", "Create") => Err(notify_error(crate::messages::MAILING_LISTS_UNSUPPORTED)),
        ("calendarMgr", "CreateAllianceEvent" | "EditAllianceEvent") => Err(notify_error(crate::messages::ALLIANCE_EVENTS_UNSUPPORTED)),
        ("accessGroupBookmarkMgr", "AddToKnownFolders" | "UpdateKnownFolderState") => Err(notify_error(crate::messages::SHARED_BOOKMARK_FOLDERS_UNSUPPORTED)),
        _ => return None,
    })
}
