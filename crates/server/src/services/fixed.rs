//! Constant replies for unmodelled remote services.
//! Mirrors client remote-service calls; no database or state is accessed.
//! Stateful and argument-dependent services are handled by their feature modules.

use super::{keyval, notify_error, rowset, Call, CallResult};
use marshal::Value;
/// Client paperdoll.State.no_recustomization.
const PAPERDOLL_NO_RECUSTOMIZATION: i64 = 0;

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

/// Fixed replies at the character position in the dispatch chain.
pub(super) fn social(call: &Call<'_>) -> Option<CallResult> {
    Some(Ok(match (call.service, call.method) {
        ("charMgr", "GetPaperdollState") => Value::Int(PAPERDOLL_NO_RECUSTOMIZATION),
        ("charMgr", "SetActivityStatus") => Value::None,
        ("charMgr", "GetCharacterDescription") => Value::unicode(""),
        ("charMgr", "GetContactList") => keyval([("addresses", rowset(&["contactID", "inWatchlist", "relationshipID", "labelMask"], vec![])), ("blocked", rowset(&["senderID"], vec![]))]),
        ("repairSvc", "RepairItemsInStation" | "RepairItemsInStructure") => Value::None,
        ("structureAssetSafety", "GetItemsInSafetyForCharacter" | "GetItemsInSafetyForCorp") => Value::list([]),
        ("bannedwords", "check_words_allowed") => Value::None,
        ("onlineStatus", "GetInitialState") => rowset(&["contactID", "online"], vec![]),
        ("corpRegistry", "GetAggressionSettings") | ("accessGroupBookmarkMgr", "AddFolder") | ("corpStationMgr", "DoStandingCheckForStationService") | ("shipAdaptionMgr", "fetch_user_data") => Value::None,
        ("RWManager", "solarsystem_contains_rw_instances") => Value::Bool(false),
        ("dynamicBountyMgr", "GetOutputForClientSolarSystem") => Value::tuple([Value::None, Value::Bool(false)]),
        _ => return None,
    }))
}
