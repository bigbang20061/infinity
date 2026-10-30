//! Location bookmarks (accessGroupBookmarkMgr, and the bound beyonce's BookmarkLocation /
//! BookmarkScanResult; eve/client/script/ui/shared/bookmarks/bookmarkSvc.py).
//!
//! Each character has personal folders (game.bookmark_folders) with subfolders, holding
//! bookmarks of startup SDE locations (regions, constellations, systems, stations), of things in
//! space, of the pilot's own position, and of scan results. A bookmark keeps the solar system
//! it is in and its coordinates. Deleting a subfolder only affects the specified owned folder;
//! a missing or mismatched subfolder returns an empty list. Shared (access-group) folders aren't
//! supported: every folder is personal.
mod access_group_bookmark_mgr;
mod beyonce;

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.service {
        "accessGroupBookmarkMgr" => access_group_bookmark_mgr::dispatch(c, call).await,
        "beyonce" => beyonce::dispatch(c, call).await,
        _ => None,
    }
}

use super::{db_err, keyval, notify_error, value_id_list, value_text, Call, CallError, CallResult};
use crate::client::{filetime_now, Client};
use crate::consts::{FILETIME_MINUTE, TYPE_SOLAR_SYSTEM};
use crate::db::Db;
use crate::dao::bookmarks as dao;
use crate::db::executor::Executor;
use crate::db::outbox::Outbox;
use crate::messages;
use marshal::Value;

/// appConst.ACCESS_PERSONAL
const ACCESS_PERSONAL: i64 = 1;
/// Server bookmark folder and memo name length limit, in characters.
const MAX_NAME: usize = 100;
/// Server bookmark and folder description limit, in characters.
const MAX_DESCRIPTION: usize = 3900;
/// Server limit on a bookmark scan-result identifier, in characters.
const MAX_SCAN_RESULT_ID: usize = 32;


fn folder_row(r: &dao::Folder, me: i64) -> Value {
    keyval([
        ("folderID", Value::Int(r.folder_id)),
        ("folderName", Value::unicode(r.name.as_str())),
        ("description", Value::unicode(r.description.as_str())),
        ("isPersonal", Value::Bool(true)),
        ("isActive", Value::Bool(true)),
        ("accessLevel", Value::Int(ACCESS_PERSONAL)),
        ("creatorID", Value::Int(me)),
        ("ownerID", Value::Int(me)),
        ("adminGroupID", Value::None),
        ("manageGroupID", Value::None),
        ("useGroupID", Value::None),
        ("viewGroupID", Value::None),
    ])
}

fn subfolder_row(r: &dao::Subfolder, me: i64) -> Value {
    keyval([
        ("subfolderID", Value::Int(r.subfolder_id)),
        ("folderID", Value::Int(r.folder_id)),
        ("subfolderName", Value::unicode(r.name.as_str())),
        ("creatorID", Value::Int(me)),
    ])
}


fn bookmark_row(r: &dao::Bookmark) -> Value {
    let opt = |v: Option<i64>| v.map(Value::Int).unwrap_or(Value::None);
    keyval([
        ("bookmarkID", Value::Int(r.bookmark_id)),
        ("folderID", Value::Int(r.folder_id)),
        ("subfolderID", opt(r.subfolder_id)),
        ("itemID", opt(r.item_id)),
        ("typeID", r.type_id.map(|t| Value::Int(t as i64)).unwrap_or(Value::None)),
        ("flag", Value::None),
        ("locationID", Value::Int(r.location_id)),
        ("x", Value::Float(r.x)),
        ("y", Value::Float(r.y)),
        ("z", Value::Float(r.z)),
        ("memo", Value::unicode(r.memo.as_str())),
        ("note", Value::unicode(r.note.as_str())),
        ("created", Value::Int(r.created)),
        ("expiry", opt(r.expiry)),
        ("creatorID", Value::Int(r.creator_id)),
    ])
}

async fn own_folder(db: &(impl Executor + ?Sized), me: i64, folder: i64) -> Result<(), CallError> {
    let mine = dao::owns_folder(db, me, folder).await.map_err(db_err)?;
    if mine {
        Ok(())
    } else {
        Err(CallError::User { msg: messages::MSG_BOOKMARK_FOLDER_NO_LONGER_THERE.into(), dict: vec![] })
    }
}

/// A bookmark at `pos` in `system`; -> the client's (bookmarkID, itemID, typeID, x, y, z,
/// locationID, expiryDate) tuple.
#[allow(clippy::too_many_arguments)]
async fn add(db: &Db, me: i64, folder: i64, subfolder: Option<i64>, item: Option<i64>, type_id: Option<i32>, system: i64, pos: [f64; 3], memo: &str, note: &str, expiry_minutes: Option<i64>) -> CallResult {
    own_folder(db, me, folder).await?;
    let now = filetime_now();
    let expiry = expiry_minutes.filter(|m| *m > 0).map(|m| now + m * FILETIME_MINUTE);
    let id = dao::add(db, &dao::NewBookmark { folder, subfolder, creator: me, item, type_id, system, pos, memo, note, created: now, expiry }).await.map_err(db_err)?;
    let opt = |v: Option<i64>| v.map(Value::Int).unwrap_or(Value::None);
    Ok(Value::tuple([
        Value::Int(id),
        opt(item),
        type_id.map(|t| Value::Int(t as i64)).unwrap_or(Value::None),
        Value::Float(pos[0]),
        Value::Float(pos[1]),
        Value::Float(pos[2]),
        Value::Int(system),
        opt(expiry),
    ]))
}

/// The expiry option the bookmark dialog passes (minutes, or None).
fn expiry(call: &Call<'_>, i: usize) -> Option<i64> {
    call.args.get(i).and_then(|v| v.as_int())
}

fn subfolder_arg(call: &Call<'_>, i: usize) -> Option<i64> {
    call.kwargs.get("subfolderID").or(call.args.get(i)).and_then(|v| v.as_int())
}




