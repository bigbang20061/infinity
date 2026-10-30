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
use crate::db::Db;
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
        db.client
            .query_opt("SELECT executor_corp_id FROM game.alliances WHERE alliance_id = $1 AND NOT deleted", &[&owner])
            .await
            .map_err(db_err)?
            .and_then(|r| r.get::<_, Option<i64>>(0))
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
    let rows = db
        .client
        .query(
            "SELECT bulletin_id, title, body, edit_char_id, edit_date, sort_order FROM game.org_bulletins WHERE owner_id = $1 ORDER BY sort_order, edit_date DESC",
            &[&owner],
        )
        .await
        .map_err(db_err)?;
    let header = bulletin_header();
    let rows = rows
        .iter()
        .map(|r| {
            header.row(&[
                Value::Int(r.get(0)),
                Value::Int(owner),
                Value::unicode(r.get::<_, String>(1)),
                Value::unicode(r.get::<_, String>(2)),
                Value::Int(r.get(3)),
                Value::Int(r.get(4)),
                Value::Int(r.get::<_, i32>(5) as i64),
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
            let current: Option<i64> = db
                .client
                .query_opt("SELECT edit_date FROM game.org_bulletins WHERE bulletin_id = $1 AND owner_id = $2", &[&id, &owner])
                .await
                .map_err(db_err)?
                .map(|r| r.get(0));
            match current {
                None => return Err(CallError::User { msg: "CorpBulletinNotFound".into(), dict: vec![] }),
                Some(t) if seen.is_some_and(|s| s < t) => return Err(CallError::User { msg: "CorpBulletinHasBeenEdited".into(), dict: vec![] }),
                Some(_) => {}
            }
            db.client
                .execute(
                    "UPDATE game.org_bulletins SET title = $2, body = $3, edit_char_id = $4, edit_date = $5 WHERE bulletin_id = $1",
                    &[&id, &title, &body, &me, &now],
                )
                .await
                .map_err(db_err)?;
        }
        None => {
            let count: i64 = db.client.query_one("SELECT count(*) FROM game.org_bulletins WHERE owner_id = $1", &[&owner]).await.map_err(db_err)?.get(0);
            if count >= MAX_BULLETINS {
                return Err(CallError::User { msg: "CorpTooManyBulletins".into(), dict: vec![] });
            }
            db.client
                .execute(
                    "INSERT INTO game.org_bulletins (owner_id, title, body, edit_char_id, edit_date, sort_order)
                     VALUES ($1, $2, $3, $4, $5, (SELECT COALESCE(max(sort_order) + 1, 0) FROM game.org_bulletins WHERE owner_id = $1))",
                    &[&owner, &title, &body, &me, &now],
                )
                .await
                .map_err(db_err)?;
        }
    }
    Ok(Value::None)
}

/// {contactID: KeyVal(contactID, relationshipID, labelMask)}
async fn contacts(db: &Db, owner: i64) -> CallResult {
    let rows = db
        .client
        .query("SELECT contact_id, relationship_id, label_mask FROM game.org_contacts WHERE owner_id = $1", &[&owner])
        .await
        .map_err(db_err)?;
    Ok(Value::Dict(
        rows.iter()
            .map(|r| {
                let id: i64 = r.get(0);
                let rel: f64 = r.get(1);
                (
                    Value::Int(id),
                    keyval([("contactID", Value::Int(id)), ("relationshipID", Value::Float(rel)), ("labelMask", Value::Int(r.get(2)))]),
                )
            })
            .collect(),
    ))
}

/// {labelID: KeyVal(labelID, name, color)}
async fn labels(db: &Db, owner: i64) -> CallResult {
    let rows = db.client.query("SELECT label_id, name, color FROM game.org_labels WHERE owner_id = $1", &[&owner]).await.map_err(db_err)?;
    Ok(Value::Dict(
        rows.iter()
            .map(|r| {
                let id: i64 = r.get(0);
                let color = r.get::<_, Option<i32>>(2).map(|c| Value::Int(c as i64)).unwrap_or(Value::None);
                (Value::Int(id), keyval([("labelID", Value::Int(id)), ("name", Value::unicode(r.get::<_, String>(1))), ("color", color)]))
            })
            .collect(),
    ))
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if !matches!(call.service, "corpRegistry" | "allianceRegistry") || (call.service == "allianceRegistry" && !call.bound) {
        return None;
    }
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
            Ok(false) => return Some(Err(notify_error("你没有执行此操作所需的职权。"))),
            Err(e) => return Some(Err(e)),
        }
    }
    let done = |r: Result<u64, tokio_postgres::Error>| r.map(|_| Value::None).map_err(db_err);
    Some(match call.method {
        "GetBulletins" => bulletins(db, owner).await,
        "AddBulletin" => add_bulletin(db, me, owner, call).await,
        "DeleteBulletin" => done(db.client.execute("DELETE FROM game.org_bulletins WHERE bulletin_id = $1 AND owner_id = $2", &[&int(0).unwrap_or(0), &owner]).await),
        // UpdateBulletinOrder([bulletinID, ...])
        "UpdateBulletinOrder" => {
            for (i, id) in value_ids(call.args.first()).iter().enumerate() {
                if let Err(e) = db.client.execute("UPDATE game.org_bulletins SET sort_order = $3 WHERE bulletin_id = $1 AND owner_id = $2", &[id, &owner, &(i as i32)]).await {
                    return Some(Err(db_err(e)));
                }
            }
            Ok(Value::None)
        }
        "GetCorporateContacts" | "GetAllianceContacts" => contacts(db, owner).await,
        // Add/Edit*Contact(contactID, relationshipID)
        "AddCorporateContact" | "AddAllianceContact" | "EditCorporateContact" | "EditAllianceContact" => done(
            db.client
                .execute(
                    "INSERT INTO game.org_contacts (owner_id, contact_id, relationship_id) VALUES ($1, $2, $3)
                     ON CONFLICT (owner_id, contact_id) DO UPDATE SET relationship_id = $3",
                    &[&owner, &int(0).unwrap_or(0), &relationship(1).unwrap_or(0.0)],
                )
                .await,
        ),
        "RemoveCorporateContacts" | "RemoveAllianceContacts" => {
            done(db.client.execute("DELETE FROM game.org_contacts WHERE owner_id = $1 AND contact_id = ANY($2)", &[&owner, &value_ids(call.args.first())]).await)
        }
        // EditContactsRelationshipID(contactIDs, relationshipID)
        "EditContactsRelationshipID" => done(
            db.client
                .execute(
                    "UPDATE game.org_contacts SET relationship_id = $3 WHERE owner_id = $1 AND contact_id = ANY($2)",
                    &[&owner, &value_ids(call.args.first()), &relationship(1).unwrap_or(0.0)],
                )
                .await,
        ),
        "GetLabels" => labels(db, owner).await,
        // CreateLabel(name, color) -> labelID
        "CreateLabel" => {
            let name = call.args.first().and_then(|v| v.as_text()).map(|t| t.trim().to_string()).unwrap_or_default();
            let color = int(1).map(|c| c as i32);
            if name.is_empty() {
                return Some(Err(CallError::User { msg: "MailLabelMustProvideName".into(), dict: vec![] }));
            }
            // A label's ID is its bit in the contacts' labelMask: the lowest free one.
            let used: Vec<i64> = match db.client.query("SELECT label_id FROM game.org_labels WHERE owner_id = $1", &[&owner]).await {
                Ok(rows) => rows.iter().map(|r| r.get(0)).collect(),
                Err(e) => return Some(Err(db_err(e))),
            };
            let Some(id) = (0..LABEL_BIT_END).map(|b| 1i64 << b).find(|b| !used.contains(b)) else {
                return Some(Err(notify_error("标签数量已达上限。")));
            };
            db.client
                .execute("INSERT INTO game.org_labels (owner_id, label_id, name, color) VALUES ($1, $2, $3, $4)", &[&owner, &id, &name, &color])
                .await
                .map(|_| Value::Int(id))
                .map_err(db_err)
        }
        "DeleteLabel" => {
            let id = int(0).unwrap_or(0);
            if let Err(e) = db.client.execute("UPDATE game.org_contacts SET label_mask = label_mask & ~$2 WHERE owner_id = $1", &[&owner, &id]).await {
                return Some(Err(db_err(e)));
            }
            done(db.client.execute("DELETE FROM game.org_labels WHERE label_id = $1 AND owner_id = $2", &[&id, &owner]).await)
        }
        // EditLabel(labelID, name, color): None keeps the current value.
        "EditLabel" => {
            let name = call.args.get(1).and_then(|v| v.as_text()).map(|t| t.trim().to_string());
            let color = int(2).map(|c| c as i32);
            done(
                db.client
                    .execute(
                        "UPDATE game.org_labels SET name = COALESCE($3, name), color = COALESCE($4, color) WHERE label_id = $1 AND owner_id = $2",
                        &[&int(0).unwrap_or(0), &owner, &name, &color],
                    )
                    .await,
            )
        }
        // AssignLabels / RemoveLabels(contactIDs, labelMask)
        "AssignLabels" | "RemoveLabels" => {
            let sql = if call.method == "AssignLabels" {
                "UPDATE game.org_contacts SET label_mask = label_mask | $3 WHERE owner_id = $1 AND contact_id = ANY($2)"
            } else {
                "UPDATE game.org_contacts SET label_mask = label_mask & ~$3 WHERE owner_id = $1 AND contact_id = ANY($2)"
            };
            done(db.client.execute(sql, &[&owner, &value_ids(call.args.first()), &int(1).unwrap_or(0)]).await)
        }
        _ => return None,
    })
}
