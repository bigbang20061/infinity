//! EVE mail and notifications (eve/client/script/ui/services/mail: mailSvc.py, notificationSvc.py).
//!
//! A message is stored once (game.mail_messages); each sender and recipient has a status row
//! (game.mail_status) with read/replied/forwarded/trashed bits and a label bitmask: Inbox,
//! Sent, Corporation and Alliance, then the character's own labels (game.mail_labels) at the
//! higher bits. The client keeps headers and bodies cached and syncs by message ID; bodies
//! travel zlib-compressed. Mail to a corporation reaches its player members; mailing lists
//! aren't supported.
//!
//! Notifications (game.notifications) are what other systems post for a character
//! (`notify`): a type, a sender and YAML data the client's formatter for that type reads.

use super::{db_err, keyval, notify_error, value_id_list, value_text, Call, CallError, CallResult};
use crate::client::{filetime_now, Client};
use crate::consts::FILETIME_MINUTE;
use crate::db::Db;
use crate::space::World;
use marshal::Value;
use std::io::Write;
use tracing::info;

/// Server sentinel for a missing notification group ID.
const MISSING_NOTIFICATION_GROUP: i32 = -1;

/// Client appConst.mailStatusMask read bit.
const STATUS_READ: i32 = 1;
/// Client appConst.mailStatusMask replied bit.
const STATUS_REPLIED: i32 = 2;
/// Client appConst.mailStatusMask forwarded bit.
const STATUS_FORWARDED: i32 = 4;
/// Client appConst.mailStatusMask trashed bit.
const STATUS_TRASHED: i32 = 8;
/// Client appConst.mailLabel inbox bit.
const LABEL_INBOX: i32 = 1;
/// Client appConst.mailLabel sent bit.
const LABEL_SENT: i32 = 2;
/// Client appConst.mailLabel corporation bit.
const LABEL_CORPORATION: i32 = 4;
/// Client appConst mail labels: lowest bit free for custom labels.
const FIRST_CUSTOM_LABEL: i32 = 16;
/// Client appConst.mailMaxRecipients.
const MAX_RECIPIENTS: usize = 50;
/// Server mail policy: maximum title characters.
const MAX_MAIL_TITLE_LENGTH: usize = 150;
/// Server mail policy: first custom-label bit position.
const FIRST_CUSTOM_LABEL_BIT: u32 = 4;
/// Server mail policy: exclusive upper custom-label bit position.
const CUSTOM_LABEL_BIT_END: u32 = 30;
/// Server notification policy: maximum rows returned.
const MAX_NOTIFICATIONS: i32 = 1000;
/// Client appConst mail labels: mask retaining the four built-in labels.
const BUILTIN_LABEL_MASK: i32 = 15;


/// The mail rows SyncMail and GetMailHeaders return.
fn header_rows(rows: &[tokio_postgres::Row]) -> Value {
    Value::list(rows.iter().map(|r| {
        keyval([
            ("messageID", Value::Int(r.get(0))),
            ("senderID", Value::Int(r.get(1))),
            ("toCharacterIDs", Value::str(r.get::<_, String>(2))),
            ("toListID", r.get::<_, Option<i64>>(3).map(Value::Int).unwrap_or(Value::None)),
            ("toCorpOrAllianceID", r.get::<_, Option<i64>>(4).map(Value::Int).unwrap_or(Value::None)),
            ("title", Value::unicode(r.get::<_, String>(5))),
            ("sentDate", Value::Int(r.get(6))),
        ])
    }))
}

const HEADER_SELECT: &str = "SELECT m.message_id, m.sender_id, m.to_character_ids, m.to_list_id, m.to_corp_or_alliance_id, m.title, m.sent_date
    FROM game.mail_messages m JOIN game.mail_status s ON s.message_id = m.message_id";

/// `SyncMail(firstID, lastID)` -> KeyVal(newMail, oldMail, mailStatus).
async fn sync(c: &Client, last_id: i64) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let new = c.db.query(&format!("{HEADER_SELECT} WHERE s.char_id = $1 AND m.message_id > $2 ORDER BY m.message_id"), &[&me, &last_id]).await.map_err(db_err)?;
    let status = c
        .db
        .query("SELECT message_id, status_mask::bigint, label_mask::bigint FROM game.mail_status WHERE char_id = $1", &[&me])
        .await
        .map_err(db_err)?;
    Ok(keyval([
        ("newMail", header_rows(&new)),
        ("oldMail", Value::None),
        (
            "mailStatus",
            Value::list(status.iter().map(|r| keyval([("messageID", Value::Int(r.get(0))), ("statusMask", Value::Int(r.get(1))), ("labelMask", Value::Int(r.get(2)))]))),
        ),
    ]))
}

/// Player members of a corporation.
async fn corp_members(db: &Db, corp: i64) -> Result<Vec<i64>, CallError> {
    Ok(super::active_corporation_member_ids(db, corp).await?.collect())
}

/// `SendMail(toCharacterIDs, toListID, toCorpOrAllianceID, title, body, isReplyTo, isForwardedFrom)` -> messageID.
async fn send(c: &Client, call: &Call<'_>) -> CallResult {
    let (db, world) = (&c.db, &*c.world);
    let me = c.session.int("charid").unwrap_or(0);
    let to_chars = value_id_list(call.args.first());
    let to_list = call.args.get(1).and_then(|v| v.as_int());
    let to_corp = call.args.get(2).and_then(|v| v.as_int());
    let title = value_text(call.args.get(3), MAX_MAIL_TITLE_LENGTH);
    let body = call.args.get(4).and_then(|v| v.as_text()).map(|t| t.into_owned()).unwrap_or_default();
    let (reply_to, forwarded_from) = (call.args.get(5).and_then(|v| v.as_int()).unwrap_or(0), call.args.get(6).and_then(|v| v.as_int()).unwrap_or(0));
    if to_list.is_some() {
        return Err(notify_error("暂不支持邮件列表。"));
    }
    if to_chars.is_empty() && to_corp.is_none() {
        return Err(notify_error("请至少选择一名收件人。"));
    }
    if to_chars.len() > MAX_RECIPIENTS {
        return Err(notify_error("收件人过多。"));
    }
    if let Some(corp) = to_corp {
        if c.session.int("corpid") != Some(corp) {
            return Err(notify_error("你只能给自己的军团发送邮件。"));
        }
    }
    let sent = filetime_now() / FILETIME_MINUTE * FILETIME_MINUTE;
    let to_text = to_chars.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",");
    let id: i64 = db
        .query_one(
            "INSERT INTO game.mail_messages (sender_id, to_character_ids, to_corp_or_alliance_id, title, body, sent_date)
             VALUES ($1, $2, $3, $4, $5, $6) RETURNING message_id",
            &[&me, &to_text, &to_corp, &title, &body, &sent],
        )
        .await
        .map_err(db_err)?
        .get(0);
    // Everyone involved gets a status row; the sender's starts read.
    let mut boxes: Vec<(i64, i32, i32)> = vec![];
    for ch in &to_chars {
        boxes.push((*ch, 0, LABEL_INBOX));
    }
    if let Some(corp) = to_corp {
        for ch in corp_members(db, corp).await? {
            match boxes.iter_mut().find(|b| b.0 == ch) {
                Some(b) => b.2 |= LABEL_CORPORATION,
                None => boxes.push((ch, 0, LABEL_CORPORATION)),
            }
        }
    }
    match boxes.iter_mut().find(|b| b.0 == me) {
        Some(b) => b.2 |= LABEL_SENT,
        None => boxes.push((me, STATUS_READ, LABEL_SENT)),
    }
    for (ch, status, labels) in &boxes {
        db
            .execute(
                "INSERT INTO game.mail_status (message_id, char_id, status_mask, label_mask) VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
                &[&id, ch, status, labels],
            )
            .await
            .map_err(db_err)?;
    }
    for (original, bit) in [(reply_to, STATUS_REPLIED), (forwarded_from, STATUS_FORWARDED)] {
        if original > 0 {
            db
                .execute("UPDATE game.mail_status SET status_mask = status_mask | $3 WHERE message_id = $1 AND char_id = $2", &[&original, &me, &bit])
                .await
                .map_err(db_err)?;
        }
    }
    let to_list_value = Value::list(to_chars.iter().map(|i| Value::Int(*i)));
    for (ch, _, _) in boxes.iter().filter(|b| b.0 != me) {
        world.notify_pilot(
            *ch,
            "OnMailSent",
            vec![
                Value::Int(id),
                Value::Int(me),
                Value::Int(sent),
                to_list_value.clone(),
                Value::None,
                to_corp.map(Value::Int).unwrap_or(Value::None),
                Value::unicode(title.as_str()),
                Value::Int(0),
            ],
        );
    }
    info!("mail: {me} sent message {id} to {} recipients", boxes.len() - 1);
    Ok(Value::Int(id))
}

/// `GetBody(messageID, markRead)` -> the zlib-compressed UTF-8 body.
async fn body(c: &Client, id: i64, mark_read: bool) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let Some(r) = c
        .db
        .query_opt("SELECT m.body FROM game.mail_messages m JOIN game.mail_status s USING (message_id) WHERE m.message_id = $1 AND s.char_id = $2", &[&id, &me])
        .await
        .map_err(db_err)?
    else {
        return Ok(Value::None);
    };
    if mark_read {
        set_bits(&c.db, me, ("message_id = ANY($2)", None), &[id], STATUS_READ, true).await?;
    }
    let text: String = r.get(0);
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    z.write_all(text.as_bytes()).map_err(|e| CallError::Internal(e.into()))?;
    Ok(Value::str(z.finish().map_err(|e| CallError::Internal(e.into()))?))
}

/// Set or clear status bits on the character's messages matching `which` ($2 = ids).
async fn set_bits(db: &Db, me: i64, (which, mask): (&str, Option<i32>), ids: &[i64], bits: i32, on: bool) -> Result<(), CallError> {
    let set = if on { "status_mask | $3::int" } else { "status_mask & ~($3::int)" };
    let sql = format!("UPDATE game.mail_status SET status_mask = {set} WHERE char_id = $1 AND {which}");
    let ids = ids.to_vec();
    if let Some(mask) = mask {
        db.execute(&sql, &[&me, &ids, &bits, &mask]).await.map_err(db_err)?;
    } else {
        db.execute(&sql, &[&me, &ids, &bits]).await.map_err(db_err)?;
    }
    Ok(())
}

/// Messages a selector covers: listed ids, a label, a mailing list (none), or all.
fn selector(method: &str) -> (&'static str, Option<i32>) {
    if method.ends_with("ByLabel") {
        ("label_mask & ($2::bigint[])[1]::int <> 0 AND status_mask & $4::int = 0", Some(STATUS_TRASHED))
    } else if method.ends_with("ByList") {
        ("false AND $2::bigint[] IS NOT NULL", None)
    } else if method.starts_with("MoveAll") || method.starts_with("MarkAll") {
        ("$2::bigint[] IS NOT NULL", None)
    } else {
        ("message_id = ANY($2)", None)
    }
}

async fn labels(c: &Client) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let rows = c.db.query("SELECT label_id::bigint, name, color::bigint FROM game.mail_labels WHERE char_id = $1", &[&me]).await.map_err(db_err)?;
    Ok(Value::Dict(
        rows.iter()
            .map(|r| {
                let id: i64 = r.get(0);
                (
                    Value::Int(id),
                    keyval([
                        ("labelID", Value::Int(id)),
                        ("name", Value::unicode(r.get::<_, String>(1))),
                        ("color", r.get::<_, Option<i64>>(2).map(Value::Int).unwrap_or(Value::None)),
                    ]),
                )
            })
            .collect(),
    ))
}

async fn create_label(c: &Client, name: &str, color: Option<i64>) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let used: Vec<i32> = c.db.query("SELECT label_id FROM game.mail_labels WHERE char_id = $1", &[&me]).await.map_err(db_err)?.iter().map(|r| r.get(0)).collect();
    let Some(id) = (FIRST_CUSTOM_LABEL_BIT..CUSTOM_LABEL_BIT_END).map(|b| 1i32 << b).filter(|b| *b >= FIRST_CUSTOM_LABEL).find(|b| !used.contains(b)) else {
        return Err(notify_error("标签数量已达上限。"));
    };
    c.db
        .execute("INSERT INTO game.mail_labels (char_id, label_id, name, color) VALUES ($1, $2, $3, $4)", &[&me, &id, &name, &color.map(|c| c as i32)])
        .await
        .map_err(db_err)?;
    Ok(Value::Int(id as i64))
}

// ---------------------------------------------------------------- notifications

/// Post a notification to a character (and show it at once if they're online). `data` is
/// the YAML mapping the client's formatter for `type_id` reads; `group` is its
/// notificationconst group.
pub async fn notify(db: &Db, world: &World, receiver: i64, type_id: i32, group: i32, sender: i64, data: &str) -> Result<(), CallError> {
    let created = filetime_now();
    let id: i64 = db
        .query_one(
            "INSERT INTO game.notifications (receiver_id, type_id, group_id, sender_id, created, data) VALUES ($1, $2, $3, $4, $5, $6) RETURNING notification_id",
            &[&receiver, &type_id, &group, &sender, &created, &data],
        )
        .await
        .map_err(db_err)?
        .get(0);
    world.notify_pilot(receiver, "OnNotificationReceived", vec![Value::Int(id), Value::Int(type_id as i64), Value::Int(sender), Value::Int(created), Value::str(data)]);
    Ok(())
}

async fn notifications(c: &Client, where_sql: &str, arg: i64) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let rows = c
        .db
        .query(
            &format!(
                "SELECT notification_id, type_id::bigint, sender_id, processed, created, data FROM game.notifications
                 WHERE receiver_id = $1 AND {where_sql} ORDER BY notification_id DESC LIMIT {MAX_NOTIFICATIONS}"
            ),
            &[&me, &arg],
        )
        .await
        .map_err(db_err)?;
    Ok(Value::list(rows.iter().map(|r| {
        keyval([
            ("notificationID", Value::Int(r.get(0))),
            ("typeID", Value::Int(r.get(1))),
            ("senderID", Value::Int(r.get(2))),
            ("receiverID", Value::Int(me)),
            ("processed", Value::Bool(r.get(3))),
            ("created", Value::Int(r.get(4))),
            ("data", Value::str(r.get::<_, String>(5))),
        ])
    })))
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let me = c.session.int("charid").unwrap_or(0);
    let int = |i: usize| call.args.get(i).and_then(|v| v.as_int());
    let db = &c.db;
    Some(match (call.service, call.method) {
        ("mailMgr", "SyncMail") => sync(c, int(1).unwrap_or(0)).await,
        ("mailMgr", "GetMailHeaders") => {
            let wanted = value_id_list(call.args.first());
            match db.query(&format!("{HEADER_SELECT} WHERE s.char_id = $1 AND m.message_id = ANY($2)"), &[&me, &wanted]).await {
                Ok(rows) => Ok(header_rows(&rows)),
                Err(e) => Err(db_err(e)),
            }
        }
        ("mailMgr", "SendMail") => send(c, call).await,
        ("mailMgr", "GetBody") => body(c, int(0).unwrap_or(0), call.args.get(1).is_some_and(|v| matches!(v, Value::Bool(true)) || v.as_int() == Some(1))).await,
        ("mailMgr", m @ ("MarkAsRead" | "MarkAsReadByLabel" | "MarkAsReadByList" | "MarkAllAsRead")) => {
            let arg = if m.ends_with("ByLabel") { vec![int(0).unwrap_or(0)] } else { value_id_list(call.args.first()) };
            set_bits(db, me, selector(m), &arg, STATUS_READ, true).await.map(|_| Value::None)
        }
        ("mailMgr", m @ ("MarkAsUnread" | "MarkAsUnreadByLabel" | "MarkAsUnreadByList" | "MarkAllAsUnread")) => {
            let arg = if m.ends_with("ByLabel") { vec![int(0).unwrap_or(0)] } else { value_id_list(call.args.first()) };
            set_bits(db, me, selector(m), &arg, STATUS_READ, false).await.map(|_| Value::None)
        }
        ("mailMgr", m @ ("MoveToTrash" | "MoveToTrashByLabel" | "MoveToTrashByList" | "MoveAllToTrash")) => {
            let arg = if m.ends_with("ByLabel") { vec![int(0).unwrap_or(0)] } else { value_id_list(call.args.first()) };
            set_bits(db, me, selector(m), &arg, STATUS_TRASHED, true).await.map(|_| Value::None)
        }
        ("mailMgr", m @ ("MoveFromTrash" | "MoveAllFromTrash")) => {
            let arg = value_id_list(call.args.first());
            set_bits(db, me, selector(m), &arg, STATUS_TRASHED, false).await.map(|_| Value::None)
        }
        ("mailMgr", "EmptyTrash") => db
            .execute("DELETE FROM game.mail_status WHERE char_id = $1 AND status_mask & $2 <> 0", &[&me, &STATUS_TRASHED])
            .await
            .map(|_| Value::None)
            .map_err(db_err),
        ("mailMgr", "DeleteMail") => {
            let wanted = value_id_list(call.args.first());
            db.execute("DELETE FROM game.mail_status WHERE char_id = $1 AND message_id = ANY($2)", &[&me, &wanted]).await.map(|_| Value::None).map_err(db_err)
        }
        ("mailMgr", "GetLabels") => labels(c).await,
        // CreateLabel(name, color) -> labelID
        ("mailMgr", "CreateLabel") => {
            let name = call.args.first().and_then(|v| v.as_text()).map(|t| t.into_owned()).unwrap_or_default();
            create_label(c, &name, int(1)).await
        }
        // EditLabel(labelID, name, color)
        ("mailMgr", "EditLabel") => {
            let name = call.args.get(1).and_then(|v| v.as_text()).map(|t| t.into_owned());
            let color = int(2).map(|c| c as i32);
            db
                .execute(
                    "UPDATE game.mail_labels SET name = COALESCE($3, name), color = COALESCE($4, color) WHERE char_id = $1 AND label_id = $2",
                    &[&me, &(int(0).unwrap_or(0) as i32), &name, &color],
                )
                .await
                .map(|_| Value::None)
                .map_err(db_err)
        }
        ("mailMgr", "DeleteLabel") => {
            let label = int(0).unwrap_or(0) as i32;
            match db.execute("DELETE FROM game.mail_labels WHERE char_id = $1 AND label_id = $2", &[&me, &label]).await {
                Ok(_) => db
                    .execute("UPDATE game.mail_status SET label_mask = label_mask & ~($2::int) WHERE char_id = $1", &[&me, &label])
                    .await
                    .map(|_| Value::None)
                    .map_err(db_err),
                Err(e) => Err(db_err(e)),
            }
        }
        // AssignLabels / RemoveLabels / ReplaceLabels([messageID], labelID)
        ("mailMgr", m @ ("AssignLabels" | "RemoveLabels" | "ReplaceLabels")) => {
            let wanted = value_id_list(call.args.first());
            let label = int(1).unwrap_or(0) as i32;
            let (set, mask) = match m {
                "AssignLabels" => ("label_mask | $3::int", None),
                "RemoveLabels" => ("label_mask & ~($3::int)", None),
                _ => ("(label_mask & $4::int) | $3::int", Some(BUILTIN_LABEL_MASK)),
            };
            let sql = format!("UPDATE game.mail_status SET label_mask = {set} WHERE char_id = $1 AND message_id = ANY($2)");
            let result = if let Some(mask) = mask {
                db.execute(&sql, &[&me, &wanted, &label, &mask]).await
            } else {
                db.execute(&sql, &[&me, &wanted, &label]).await
            };
            result.map(|_| Value::None).map_err(db_err)
        }
        ("mailingListsMgr", "Create") => Err(notify_error("暂不支持邮件列表。")),

        ("notificationMgr", "GetAllNotifications") => {
            let from = call.kwargs.get("fromID").and_then(|v| v.as_int()).or(int(0)).unwrap_or(0);
            notifications(c, "notification_id > $2", from).await
        }
        ("notificationMgr", "GetUnprocessed") => notifications(c, "NOT processed AND $2::bigint IS NOT NULL", 0).await,
        ("notificationMgr", "GetByGroupID") => notifications(c, "group_id::bigint = $2", int(0).unwrap_or(MISSING_NOTIFICATION_GROUP as i64)).await,
        ("notificationMgr", "MarkAsProcessed") => {
            let wanted = value_id_list(call.args.first());
            db
                .execute("UPDATE game.notifications SET processed = true WHERE receiver_id = $1 AND notification_id = ANY($2)", &[&me, &wanted])
                .await
                .map(|_| Value::None)
                .map_err(db_err)
        }
        ("notificationMgr", "MarkGroupAsProcessed") => db
            .execute("UPDATE game.notifications SET processed = true WHERE receiver_id = $1 AND group_id = $2", &[&me, &(int(0).unwrap_or(MISSING_NOTIFICATION_GROUP as i64) as i32)])
            .await
            .map(|_| Value::None)
            .map_err(db_err),
        ("notificationMgr", "MarkAllAsProcessed") => {
            db.execute("UPDATE game.notifications SET processed = true WHERE receiver_id = $1", &[&me]).await.map(|_| Value::None).map_err(db_err)
        }
        ("notificationMgr", "DeleteNotifications") => {
            let wanted = value_id_list(call.args.first());
            db
                .execute("DELETE FROM game.notifications WHERE receiver_id = $1 AND notification_id = ANY($2)", &[&me, &wanted])
                .await
                .map(|_| Value::None)
                .map_err(db_err)
        }
        ("notificationMgr", "DeleteGroupNotifications") => db
            .execute("DELETE FROM game.notifications WHERE receiver_id = $1 AND group_id = $2", &[&me, &(int(0).unwrap_or(MISSING_NOTIFICATION_GROUP as i64) as i32)])
            .await
            .map(|_| Value::None)
            .map_err(db_err),
        ("notificationMgr", "DeleteAllNotifications") => {
            db.execute("DELETE FROM game.notifications WHERE receiver_id = $1", &[&me]).await.map(|_| Value::None).map_err(db_err)
        }
        ("notificationMgr", "LogNotificationInteraction") => Ok(Value::None),
        _ => return None,
    })
}
