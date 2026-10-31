//! `mailMgr`, mirroring `eve/client/script/ui/services/mail/mailSvc.py`.
//! Uses the mail DAO's message, status and label tables and character membership.
//! Sending commits message, status and reply/forward writes together before notifications.
//! Mailing lists are not supported.
use super::super::{db_err, keyval, notify_error, value_id_list, Call, CallError, CallResult};
use crate::client::{filetime_now, Client};
use crate::consts::FILETIME_MINUTE;
use crate::dao::mail::{self, LabelAction, Selector};
use crate::db::Db;
use crate::db::outbox::{dispatch_world, Outbox};
use crate::messages;
use crate::space::World;
use marshal::Value;
use std::io::Write;
use tracing::info;

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
/// Server mail policy: first custom-label bit position.
const FIRST_CUSTOM_LABEL_BIT: u32 = 4;
/// Server mail policy: exclusive upper custom-label bit position.
const CUSTOM_LABEL_BIT_END: u32 = 30;
/// Client appConst mail labels: mask retaining the four built-in labels.
const BUILTIN_LABEL_MASK: i32 = 15;


/// The mail rows SyncMail and GetMailHeaders return.
fn header_rows(rows: &[mail::Header]) -> Value {
    Value::list(rows.iter().map(|r| {
        keyval([
            ("messageID", Value::Int(r.message_id)),
            ("senderID", Value::Int(r.sender_id)),
            ("toCharacterIDs", Value::str(r.to_character_ids.as_str())),
            ("toListID", r.to_list_id.map(Value::Int).unwrap_or(Value::None)),
            ("toCorpOrAllianceID", r.to_corp_or_alliance_id.map(Value::Int).unwrap_or(Value::None)),
            ("title", Value::unicode(r.title.as_str())),
            ("sentDate", Value::Int(r.sent_date)),
        ])
    }))
}


/// `SyncMail(firstID, lastID)` -> KeyVal(newMail, oldMail, mailStatus).
async fn sync(c: &Client, last_id: i64) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let new = mail::new_headers(&c.db, me, last_id).await.map_err(db_err)?;
    let status = mail::statuses(&c.db, me).await.map_err(db_err)?;
    Ok(keyval([
        ("newMail", header_rows(&new)),
        ("oldMail", Value::None),
        (
            "mailStatus",
            Value::list(status.iter().map(|r| keyval([("messageID", Value::Int(r.message_id)), ("statusMask", Value::Int(r.status_mask)), ("labelMask", Value::Int(r.label_mask))]))),
        ),
    ]))
}

/// Player members of a corporation.
async fn corp_members(db: &Db, corp: i64) -> Result<Vec<i64>, CallError> {
    crate::dao::characters::members::active_ids(db, corp).await.map_err(db_err)
}

/// `SendMail(toCharacterIDs, toListID, toCorpOrAllianceID, title, body, isReplyTo, isForwardedFrom)` -> messageID.
async fn send(c: &Client, call: &Call<'_>) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let to_chars = value_id_list(call.args.first());
    let to_list = call.args.get(1).and_then(|v| v.as_int());
    let to_corp = call.args.get(2).and_then(|v| v.as_int());
    let title = call.args.get(3).and_then(|v| v.as_text()).map(|t| t.into_owned()).unwrap_or_default();
    let body = call.args.get(4).and_then(|v| v.as_text()).map(|t| t.into_owned()).unwrap_or_default();
    let (reply_to, forwarded_from) = (call.args.get(5).and_then(|v| v.as_int()).unwrap_or(0), call.args.get(6).and_then(|v| v.as_int()).unwrap_or(0));
    let input = SendMail { to_chars, to_list, to_corp, title, body, reply_to, forwarded_from };
    send_mail(&c.db, &c.world, me, c.session.int("corpid"), input).await.map(Value::Int)
}

pub(crate) struct SendMail {
    pub to_chars: Vec<i64>,
    pub to_list: Option<i64>,
    pub to_corp: Option<i64>,
    pub title: String,
    pub body: String,
    pub reply_to: i64,
    pub forwarded_from: i64,
}

pub(crate) async fn send_mail(db: &Db, world: &World, me: i64, corpid: Option<i64>, input: SendMail) -> Result<i64, CallError> {
    let SendMail { to_chars, to_list, to_corp, title, body, reply_to, forwarded_from } = input;
    let title = title.chars().take(usize::try_from(crate::config::game().mail.title_max_len).unwrap_or(usize::MAX)).collect::<String>();
    if to_list.is_some() {
        return Err(notify_error(messages::MAILING_LISTS_UNSUPPORTED));
    }
    if to_chars.is_empty() && to_corp.is_none() {
        return Err(notify_error(messages::MAIL_RECIPIENT_REQUIRED));
    }
    if to_chars.len() > MAX_RECIPIENTS {
        return Err(notify_error(messages::TOO_MANY_MAIL_RECIPIENTS));
    }
    if let Some(corp) = to_corp {
        if corpid != Some(corp) {
            return Err(notify_error(messages::MAIL_ONLY_TO_OWN_CORPORATION));
        }
    }
    let sent = filetime_now() / FILETIME_MINUTE * FILETIME_MINUTE;
    let to_text = to_chars.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",");
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
    // Stable sorting preserves which status wins for duplicate recipients.
    let mut sorted_boxes = boxes.clone();
    sorted_boxes.sort_by_key(|b| b.0);
    let mut originals = [(reply_to, STATUS_REPLIED), (forwarded_from, STATUS_FORWARDED)];
    originals.sort_by_key(|(id, _)| *id);
    let to_list_value = Value::list(to_chars.iter().map(|i| Value::Int(*i)));
    let inputs = std::sync::Arc::new((to_text, title, body, boxes, sorted_boxes, to_list_value));
    let id = db.transaction(
        |tx, outbox: &mut Outbox| {
            let inputs = std::sync::Arc::clone(&inputs);
            Box::pin(async move {
                let (to_text, title, body, boxes, sorted_boxes, to_list_value) = &*inputs;
                let id = mail::insert_message(tx, me, to_text, to_corp, title, body, sent).await.map_err(db_err)?;
                for (ch, status, labels) in sorted_boxes {
                    mail::insert_status(tx, id, *ch, *status, *labels).await.map_err(db_err)?;
                }
                for (original, bit) in originals {
                    if original > 0 {
                        mail::mark_original(tx, original, me, bit).await.map_err(db_err)?;
                    }
                }
                for (ch, _, _) in boxes.iter().filter(|b| b.0 != me) {
                    outbox.notify_pilot(
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
                Ok(id)
            })
        },
        |id, outbox| async move { dispatch_world(id, outbox, world) },
    ).await?;
    info!("mail: {me} sent message {id} to {} recipients", inputs.3.len() - 1);
    Ok(id)
}

/// `GetBody(messageID, markRead)` -> the zlib-compressed UTF-8 body.
async fn body(c: &Client, id: i64, mark_read: bool) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let Some(text) = mail::body(&c.db, id, me).await.map_err(db_err)?
    else {
        return Ok(Value::None);
    };
    if mark_read {
        set_bits(&c.db, me, Selector::Ids, &[id], STATUS_READ, true).await?;
    }
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    z.write_all(text.as_bytes()).map_err(|e| CallError::Internal(e.into()))?;
    Ok(Value::str(z.finish().map_err(|e| CallError::Internal(e.into()))?))
}

/// Set or clear status bits on the character's messages matching `which` ($2 = ids).
async fn set_bits(db: &Db, me: i64, which: Selector, ids: &[i64], bits: i32, on: bool) -> Result<(), CallError> {
    mail::set_bits(db, me, which, ids, bits, on, STATUS_TRASHED).await.map_err(db_err)?;
    Ok(())
}

/// Messages a selector covers: listed ids, a label, a mailing list (none), or all.
fn selector(method: &str) -> Selector {
    if method.ends_with("ByLabel") {
        Selector::Label
    } else if method.ends_with("ByList") {
        Selector::List
    } else if method.starts_with("MoveAll") || method.starts_with("MarkAll") {
        Selector::All
    } else {
        Selector::Ids
    }
}

async fn labels(c: &Client) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let rows = mail::labels(&c.db, me).await.map_err(db_err)?;
    Ok(Value::Dict(
        rows.iter()
            .map(|r| {
                let id = r.label_id;
                (
                    Value::Int(id),
                    keyval([
                        ("labelID", Value::Int(id)),
                        ("name", Value::unicode(r.name.as_str())),
                        ("color", r.color.map(Value::Int).unwrap_or(Value::None)),
                    ]),
                )
            })
            .collect(),
    ))
}

async fn create_label(c: &Client, name: &str, color: Option<i64>) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let used = mail::label_ids(&c.db, me).await.map_err(db_err)?;
    let Some(id) = (FIRST_CUSTOM_LABEL_BIT..CUSTOM_LABEL_BIT_END).map(|b| 1i32 << b).filter(|b| *b >= FIRST_CUSTOM_LABEL).find(|b| !used.contains(b)) else {
        return Err(notify_error(messages::LABEL_LIMIT_REACHED));
    };
    mail::insert_label(&c.db, me, id, name, color.map(|c| c as i32)).await.map_err(db_err)?;
    Ok(Value::Int(id as i64))
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let me = c.session.int("charid").unwrap_or(0);
    let int = |i: usize| call.args.get(i).and_then(|v| v.as_int());
    let db = &c.db;
    Some(match (call.service, call.method) {
        ("mailMgr", "SyncMail") => sync(c, int(1).unwrap_or(0)).await,
        ("mailMgr", "GetMailHeaders") => {
            let wanted = value_id_list(call.args.first());
            match mail::headers(db, me, &wanted).await {
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
        ("mailMgr", "EmptyTrash") => mail::empty_trash(db, me, STATUS_TRASHED).await.map(|_| Value::None).map_err(db_err),
        ("mailMgr", "DeleteMail") => {
            let wanted = value_id_list(call.args.first());
            mail::delete_mail(db, me, &wanted).await.map(|_| Value::None).map_err(db_err)
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
            mail::edit_label(db, me, int(0).unwrap_or(0) as i32, name, color).await.map(|_| Value::None).map_err(db_err)
        }
        ("mailMgr", "DeleteLabel") => {
            let label = int(0).unwrap_or(0) as i32;
            match mail::delete_label(db, me, label).await {
                Ok(_) => mail::clear_label(db, me, label)
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
            let action = match m {
                "AssignLabels" => LabelAction::Assign,
                "RemoveLabels" => LabelAction::Remove,
                _ => LabelAction::Replace,
            };
            let result = mail::change_labels(db, me, &wanted, label, action, BUILTIN_LABEL_MASK).await;
            result.map(|_| Value::None).map_err(db_err)
        }
        _ => return None,
    })
}
