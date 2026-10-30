//! Bulletins, contacts and contact labels of corporations (corpRegistry) and alliances
//! (bound allianceRegistry), kept per owner in game.org_*.
//!
//! A corporation's communications officers (corpRoleChatManager) and directors edit its
//! bulletins, and its diplomats and directors its contacts and labels; for an alliance,
//! those of its executor corporation do.

use super::corps::membership;
use super::{crowset, db_err, keyval, notify_error, value_ids, Call, CallError, CallResult};
use crate::client::{filetime_now, Client};
use crate::consts::ROLE_DIRECTOR;
use crate::dao::{alliances, orgs};
use crate::db::Db;
use crate::messages;

mod alliance_registry;
mod corp_registry;
use marshal::{DbRowDescriptor, DbType, Value};

/// Client appConst.corpRoleChatManager.
const ROLE_CHAT_MANAGER: i64 = 36_028_797_018_963_968;
/// Client appConst.corpRoleDiplomat.
const ROLE_DIPLOMAT: i64 = 17_179_869_184;
/// Player alliance ID range, matching the server's game.alliances sequence.
const FIRST_PLAYER_ALLIANCE: i64 = 99_000_000;
/// base_corporation.MAX_NUM_BULLETINS
const MAX_BULLETINS: i64 = 10;
/// Server label-mask policy: exclusive upper bit position for positive i64 labels.
const LABEL_BIT_END: u32 = 62;

/// The owner a call is for: the bound corporation, or the bound alliance.
fn owner(c: &Client, call: &Call<'_>) -> Option<i64> {
    let bound = call.bind_params.and_then(|p| p.as_int().or_else(|| p.items().and_then(|t| t.first()?.as_int())));
    match call.service {
        "corpRegistry" => bound.or_else(|| c.session.int("corpid")),
        _ => bound.or_else(|| c.session.int("allianceid")),
    }
}

/// Whether `me` holds one of `roles` in `owner` (a corporation) or in its executor
/// corporation (an alliance).
async fn may(db: &Db, me: i64, owner: i64, roles: i64) -> Result<bool, CallError> {
    let (corp, my_roles) = membership(db, me).await?;
    let acting_corp = if owner >= FIRST_PLAYER_ALLIANCE {
        alliances::executor_corp(db, owner).await.map_err(db_err)?
    } else {
        Some(owner)
    };
    Ok(acting_corp == Some(corp) && my_roles & (roles | ROLE_DIRECTOR) != 0)
}

fn bulletin_header() -> DbRowDescriptor {
    DbRowDescriptor::new([
        ("bulletinID", DbType::I4),
        ("ownerID", DbType::I4),
        ("title", DbType::WStr),
        ("body", DbType::WStr),
        ("editCharacterID", DbType::I4),
        ("editDateTime", DbType::FileTime),
        ("sortOrder", DbType::I4),
    ])
}

async fn bulletins(db: &Db, owner: i64) -> CallResult {
    let rows = orgs::bulletins(db, owner).await.map_err(db_err)?;
    let header = bulletin_header();
    let rows = rows
        .into_iter()
        .map(|r| {
            header.row(&[
                Value::Int(r.id),
                Value::Int(owner),
                Value::unicode(r.title),
                Value::unicode(r.body),
                Value::Int(r.edit_char_id),
                Value::Int(r.edit_date),
                Value::Int(r.sort_order as i64),
            ])
        })
        .collect();
    Ok(crowset(&header, rows))
}

/// AddBulletin(title, body, bulletinID=None, editDateTime=None): a new bulletin, or an edit of
/// `bulletinID` unless someone saved it after `editDateTime`.
async fn add_bulletin(db: &Db, me: i64, owner: i64, call: &Call<'_>) -> CallResult {
    let text = |i: usize| call.args.get(i).and_then(|v| v.as_text()).map(|t| t.into_owned()).unwrap_or_default();
    let (title, body) = (text(0), text(1));
    let edit_of = call.kwargs.get("bulletinID").or_else(|| call.args.get(2)).and_then(|v| v.as_int());
    let seen = call.kwargs.get("editDateTime").or_else(|| call.args.get(3)).and_then(|v| v.as_int());
    let now = filetime_now();
    match edit_of {
        Some(id) => {
            let current = orgs::edit_date(db, id, owner).await.map_err(db_err)?;
            match current {
                None => return Err(CallError::User { msg: messages::MSG_CORP_BULLETIN_NOT_FOUND.into(), dict: vec![] }),
                Some(t) if seen.is_some_and(|s| s < t) => return Err(CallError::User { msg: messages::MSG_CORP_BULLETIN_HAS_BEEN_EDITED.into(), dict: vec![] }),
                Some(_) => {}
            }
            orgs::update_bulletin(db, id, &title, &body, me, now).await.map_err(db_err)?;
        }
        None => {
            let count = orgs::bulletin_count(db, owner).await.map_err(db_err)?;
            if count >= MAX_BULLETINS {
                return Err(CallError::User { msg: messages::MSG_CORP_TOO_MANY_BULLETINS.into(), dict: vec![] });
            }
            orgs::insert_bulletin(db, owner, &title, &body, me, now).await.map_err(db_err)?;
        }
    }
    Ok(Value::None)
}

/// {contactID: KeyVal(contactID, relationshipID, labelMask)}
async fn contacts(db: &Db, owner: i64) -> CallResult {
    let rows = orgs::contacts(db, owner).await.map_err(db_err)?;
    Ok(Value::Dict(
        rows.iter()
            .map(|r| {
                let id = r.id;
                let rel = r.relationship;
                (
                    Value::Int(id),
                    keyval([("contactID", Value::Int(id)), ("relationshipID", Value::Float(rel)), ("labelMask", Value::Int(r.label_mask))]),
                )
            })
            .collect(),
    ))
}

/// {labelID: KeyVal(labelID, name, color)}
async fn labels(db: &Db, owner: i64) -> CallResult {
    let rows = orgs::labels(db, owner).await.map_err(db_err)?;
    Ok(Value::Dict(
        rows.into_iter()
            .map(|r| {
                let id = r.id;
                let color = r.color.map(|c| Value::Int(c as i64)).unwrap_or(Value::None);
                (Value::Int(id), keyval([("labelID", Value::Int(id)), ("name", Value::unicode(r.name)), ("color", color)]))
            })
            .collect(),
    ))
}

async fn dispatch_shared(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let me = c.session.int("charid").unwrap_or(0);
    let db = &c.db;
    let owner = owner(c, call)?;
    let int = |i: usize| call.args.get(i).and_then(|v| v.as_int());
    let relationship = |i: usize| call.kwargs.get("relationshipID").or_else(|| call.args.get(i)).and_then(|v| v.as_float().or_else(|| v.as_int().map(|i| i as f64)));
    // The edits below need these roles.
    let needed = match call.method {
        "AddBulletin" | "DeleteBulletin" | "UpdateBulletinOrder" => Some(ROLE_CHAT_MANAGER),
        "AddCorporateContact" | "AddAllianceContact" | "EditCorporateContact" | "EditAllianceContact" | "RemoveCorporateContacts"
        | "RemoveAllianceContacts" | "EditContactsRelationshipID" | "CreateLabel" | "DeleteLabel" | "EditLabel" | "AssignLabels" | "RemoveLabels" => {
            Some(ROLE_DIPLOMAT)
        }
        "GetBulletins" | "GetCorporateContacts" | "GetAllianceContacts" | "GetLabels" => None,
        _ => return None,
    };
    if let Some(roles) = needed {
        match may(db, me, owner, roles).await {
            Ok(true) => {}
            Ok(false) => return Some(Err(notify_error(messages::INSUFFICIENT_ROLE_FOR_OPERATION))),
            Err(e) => return Some(Err(e)),
        }
    }
    let done = |r: Result<u64, crate::db::Error>| r.map(|_| Value::None).map_err(db_err);
    Some(match call.method {
        "GetBulletins" => bulletins(db, owner).await,
        "AddBulletin" => add_bulletin(db, me, owner, call).await,
        "DeleteBulletin" => done(orgs::delete_bulletin(db, int(0).unwrap_or(0), owner).await),
        // UpdateBulletinOrder([bulletinID, ...])
        "UpdateBulletinOrder" => orgs::order_bulletins(db, owner, &value_ids(call.args.first())).await.map(|_| Value::None).map_err(db_err),
        "GetCorporateContacts" | "GetAllianceContacts" => contacts(db, owner).await,
        // Add/Edit*Contact(contactID, relationshipID)
        "AddCorporateContact" | "AddAllianceContact" | "EditCorporateContact" | "EditAllianceContact" => done(
            orgs::upsert_contact(db, owner, int(0).unwrap_or(0), relationship(1).unwrap_or(0.0)).await,
        ),
        "RemoveCorporateContacts" | "RemoveAllianceContacts" => {
            done(orgs::delete_contacts(db, owner, &value_ids(call.args.first())).await)
        }
        // EditContactsRelationshipID(contactIDs, relationshipID)
        "EditContactsRelationshipID" => done(
            orgs::update_relationship(db, owner, &value_ids(call.args.first()), relationship(1).unwrap_or(0.0)).await,
        ),
        "GetLabels" => labels(db, owner).await,
        // CreateLabel(name, color) -> labelID
        "CreateLabel" => {
            let name = call.args.first().and_then(|v| v.as_text()).map(|t| t.trim().to_string()).unwrap_or_default();
            let color = int(1).map(|c| c as i32);
            if name.is_empty() {
                return Some(Err(CallError::User { msg: messages::MSG_MAIL_LABEL_MUST_PROVIDE_NAME.into(), dict: vec![] }));
            }
            // A label's ID is its bit in the contacts' labelMask: the lowest free one.
            let used: Vec<i64> = match orgs::label_ids(db, owner).await {
                Ok(rows) => rows,
                Err(e) => return Some(Err(db_err(e))),
            };
            let Some(id) = (0..LABEL_BIT_END).map(|b| 1i64 << b).find(|b| !used.contains(b)) else {
                return Some(Err(notify_error(messages::LABEL_LIMIT_REACHED)));
            };
            orgs::insert_label(db, owner, id, &name, color).await
                .map(|_| Value::Int(id))
                .map_err(db_err)
        }
        "DeleteLabel" => {
            let id = int(0).unwrap_or(0);
            if let Err(e) = orgs::clear_label(db, owner, id).await {
                return Some(Err(db_err(e)));
            }
            done(orgs::delete_label(db, id, owner).await)
        }
        // EditLabel(labelID, name, color): None keeps the current value.
        "EditLabel" => {
            let name = call.args.get(1).and_then(|v| v.as_text()).map(|t| t.trim().to_string());
            let color = int(2).map(|c| c as i32);
            done(
                orgs::edit_label(db, int(0).unwrap_or(0), owner, name, color).await,
            )
        }
        // AssignLabels / RemoveLabels(contactIDs, labelMask)
        "AssignLabels" | "RemoveLabels" => {
            let change = if call.method == "AssignLabels" { orgs::LabelChange::Assign } else { orgs::LabelChange::Remove };
            done(orgs::change_labels(db, owner, &value_ids(call.args.first()), int(1).unwrap_or(0), change).await)
        }
        _ => return None,
    })
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.service {
        "corpRegistry" => corp_registry::dispatch(c, call).await,
        "allianceRegistry" => alliance_registry::dispatch(c, call).await,
        _ => None,
    }
}
