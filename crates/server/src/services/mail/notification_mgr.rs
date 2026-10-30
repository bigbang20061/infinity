//! `notificationMgr`, mirroring `eve/client/script/ui/services/mail/notificationSvc.py`.
//! Reads and updates `game.notifications` via the mail DAO. Notification interaction
//! logging is not modelled.
use super::super::{db_err, keyval, value_id_list, Call, CallResult};
use crate::client::Client;
use crate::dao::mail::{self, NotificationFilter, NotificationSelection};
use marshal::Value;
/// Server sentinel for a missing notification group ID.
const MISSING_NOTIFICATION_GROUP: i32 = -1;

async fn notifications(c: &Client, filter: NotificationFilter, arg: i64) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let rows = mail::notifications(&c.db, me, filter, arg).await.map_err(db_err)?;
    Ok(Value::list(rows.iter().map(|r| {
        keyval([
            ("notificationID", Value::Int(r.notification_id)),
            ("typeID", Value::Int(r.type_id)),
            ("senderID", Value::Int(r.sender_id)),
            ("receiverID", Value::Int(me)),
            ("processed", Value::Bool(r.processed)),
            ("created", Value::Int(r.created)),
            ("data", Value::str(r.data.as_str())),
        ])
    })))
}
pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let me = c.session.int("charid").unwrap_or(0);
    let int = |i: usize| call.args.get(i).and_then(|v| v.as_int());
    let db = &c.db;
    Some(match (call.service, call.method) {
        ("notificationMgr", "GetAllNotifications") => {
            let from = call.kwargs.get("fromID").and_then(|v| v.as_int()).or(int(0)).unwrap_or(0);
            notifications(c, NotificationFilter::After, from).await
        }
        ("notificationMgr", "GetUnprocessed") => notifications(c, NotificationFilter::Unprocessed, 0).await,
        ("notificationMgr", "GetByGroupID") => notifications(c, NotificationFilter::Group, int(0).unwrap_or(MISSING_NOTIFICATION_GROUP as i64)).await,
        ("notificationMgr", "MarkAsProcessed") => {
            let wanted = value_id_list(call.args.first());
            mail::process_notifications(db, me, NotificationSelection::Ids(&wanted)).await.map(|_| Value::None).map_err(db_err)
        }
        ("notificationMgr", "MarkGroupAsProcessed") => mail::process_notifications(db, me, NotificationSelection::Group(int(0).unwrap_or(MISSING_NOTIFICATION_GROUP as i64) as i32)).await.map(|_| Value::None).map_err(db_err),
        ("notificationMgr", "MarkAllAsProcessed") => {
            mail::process_notifications(db, me, NotificationSelection::All).await.map(|_| Value::None).map_err(db_err)
        }
        ("notificationMgr", "DeleteNotifications") => {
            let wanted = value_id_list(call.args.first());
            mail::delete_notifications(db, me, NotificationSelection::Ids(&wanted)).await.map(|_| Value::None).map_err(db_err)
        }
        ("notificationMgr", "DeleteGroupNotifications") => mail::delete_notifications(db, me, NotificationSelection::Group(int(0).unwrap_or(MISSING_NOTIFICATION_GROUP as i64) as i32)).await.map(|_| Value::None).map_err(db_err),
        ("notificationMgr", "DeleteAllNotifications") => {
            mail::delete_notifications(db, me, NotificationSelection::All).await.map(|_| Value::None).map_err(db_err)
        }
        _ => return None,
    })
}
