//! Location bookmarks (accessGroupBookmarkMgr, and the bound beyonce's BookmarkLocation /
//! BookmarkScanResult; eve/client/script/ui/shared/bookmarks/bookmarkSvc.py).
//!
//! Each character has personal folders (game.bookmark_folders) with subfolders, holding
//! bookmarks of static locations (regions, constellations, systems, stations), of things in
//! space, of the pilot's own position, and of scan results. A bookmark keeps the solar system
//! it is in and its coordinates. Deleting a subfolder only affects the specified owned folder;
//! a missing or mismatched subfolder returns an empty list. Shared (access-group) folders aren't
//! supported: every folder is personal.

use super::{db_err, keyval, notify_error, value_id_list, value_text, Call, CallError, CallResult};
use crate::client::{filetime_now, Client};
use crate::consts::{FILETIME_MINUTE, TYPE_SOLAR_SYSTEM};
use crate::db::Db;
use crate::db::executor::Executor;
use crate::db::outbox::Outbox;
use marshal::Value;

/// appConst.ACCESS_PERSONAL
const ACCESS_PERSONAL: i64 = 1;
/// Server bookmark folder and memo name length limit, in characters.
const MAX_NAME: usize = 100;
/// Server bookmark and folder description limit, in characters.
const MAX_DESCRIPTION: usize = 3900;
/// Server limit on a bookmark scan-result identifier, in characters.
const MAX_SCAN_RESULT_ID: usize = 32;


fn folder_row(r: &tokio_postgres::Row, me: i64) -> Value {
    keyval([
        ("folderID", Value::Int(r.get(0))),
        ("folderName", Value::unicode(r.get::<_, String>(1))),
        ("description", Value::unicode(r.get::<_, String>(2))),
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

fn subfolder_row(r: &tokio_postgres::Row, me: i64) -> Value {
    keyval([
        ("subfolderID", Value::Int(r.get(0))),
        ("folderID", Value::Int(r.get(1))),
        ("subfolderName", Value::unicode(r.get::<_, String>(2))),
        ("creatorID", Value::Int(me)),
    ])
}

const BOOKMARK_SELECT: &str = "SELECT bookmark_id, folder_id, subfolder_id, item_id, type_id, location_id, x, y, z, memo, note, created, expiry, creator_id
    FROM game.bookmarks";

fn bookmark_row(r: &tokio_postgres::Row) -> Value {
    let opt = |v: Option<i64>| v.map(Value::Int).unwrap_or(Value::None);
    keyval([
        ("bookmarkID", Value::Int(r.get(0))),
        ("folderID", Value::Int(r.get(1))),
        ("subfolderID", opt(r.get(2))),
        ("itemID", opt(r.get(3))),
        ("typeID", r.get::<_, Option<i32>>(4).map(|t| Value::Int(t as i64)).unwrap_or(Value::None)),
        ("flag", Value::None),
        ("locationID", Value::Int(r.get(5))),
        ("x", Value::Float(r.get(6))),
        ("y", Value::Float(r.get(7))),
        ("z", Value::Float(r.get(8))),
        ("memo", Value::unicode(r.get::<_, String>(9))),
        ("note", Value::unicode(r.get::<_, String>(10))),
        ("created", Value::Int(r.get(11))),
        ("expiry", opt(r.get(12))),
        ("creatorID", Value::Int(r.get(13))),
    ])
}

async fn own_folder(db: &(impl Executor + ?Sized), me: i64, folder: i64) -> Result<(), CallError> {
    let mine: bool = db
        .query_one("SELECT EXISTS (SELECT 1 FROM game.bookmark_folders WHERE folder_id = $1 AND owner_id = $2)", &[&folder, &me])
        .await
        .map_err(db_err)?
        .get(0);
    if mine {
        Ok(())
    } else {
        Err(CallError::User { msg: "BookmarkFolderNoLongerThere".into(), dict: vec![] })
    }
}

/// A bookmark at `pos` in `system`; -> the client's (bookmarkID, itemID, typeID, x, y, z,
/// locationID, expiryDate) tuple.
#[allow(clippy::too_many_arguments)]
async fn add(db: &Db, me: i64, folder: i64, subfolder: Option<i64>, item: Option<i64>, type_id: Option<i32>, system: i64, pos: [f64; 3], memo: &str, note: &str, expiry_minutes: Option<i64>) -> CallResult {
    own_folder(db, me, folder).await?;
    let now = filetime_now();
    let expiry = expiry_minutes.filter(|m| *m > 0).map(|m| now + m * FILETIME_MINUTE);
    let id: i64 = db
        .query_one(
            "INSERT INTO game.bookmarks (folder_id, subfolder_id, creator_id, item_id, type_id, location_id, x, y, z, memo, note, created, expiry)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) RETURNING bookmark_id",
            &[&folder, &subfolder, &me, &item, &type_id, &system, &pos[0], &pos[1], &pos[2], &memo, &note, &now, &expiry],
        )
        .await
        .map_err(db_err)?
        .get(0);
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

/// `BookmarkStaticLocation(itemID, folderID, name, comment, expiry, subfolderID=)`: a region,
/// constellation, solar system or station.
async fn static_location(c: &Client, call: &Call<'_>) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let item = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
    let r = c
        .db
        .query_opt(
            "SELECT s.solar_system_id::bigint, s.type_id, c.x, c.y, c.z FROM sde.sta_stations s JOIN sde.map_celestials c ON c.item_id = s.station_id::bigint
             WHERE s.station_id::bigint = $1
             UNION ALL SELECT solar_system_id::bigint, $2, 0, 0, 0 FROM sde.map_solar_systems WHERE solar_system_id::bigint = $1",
            &[&item, &TYPE_SOLAR_SYSTEM],
        )
        .await
        .map_err(db_err)?;
    // Regions and constellations bookmark the solar system the pilot is in.
    let (system, type_id, pos) = match r {
        Some(r) => (r.get::<_, i64>(0), Some(r.get::<_, i32>(1)), [r.get(2), r.get(3), r.get(4)]),
        None => (c.session.int("solarsystemid2").unwrap_or(0), Some(TYPE_SOLAR_SYSTEM), [0.0; 3]),
    };
    let folder = call.args.get(1).and_then(|v| v.as_int()).unwrap_or(0);
    add(&c.db, me, folder, subfolder_arg(call, 5), Some(item), type_id, system, pos, &value_text(call.args.get(2), MAX_NAME), &value_text(call.args.get(3), MAX_DESCRIPTION), expiry(call, 4)).await
}

/// beyonce `BookmarkLocation(itemID, folderID, name, comment, expiry, subfolderID=)`: something in
/// space, or (the pilot's own ship) the spot they are at.
async fn space_location(c: &Client, call: &Call<'_>) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let Some(system) = c.session.int("solarsystemid") else {
        return Err(CallError::User { msg: "BkmMustBeInFlight".into(), dict: vec![] });
    };
    let item = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
    let Some(ball) = c.world.ball(system, item).await else {
        return Err(notify_error("目标已不在原处。"));
    };
    let own = Some(item) == c.session.int("shipid");
    let (item, type_id) = if own { (None, Some(TYPE_SOLAR_SYSTEM)) } else { (Some(item), Some(ball.type_id)) };
    let folder = call.args.get(1).and_then(|v| v.as_int()).unwrap_or(0);
    add(&c.db, me, folder, subfolder_arg(call, 5), item, type_id, system, ball.pos, &value_text(call.args.get(2), MAX_NAME), &value_text(call.args.get(3), MAX_DESCRIPTION), expiry(call, 4)).await
}

/// beyonce `BookmarkScanResult(locationID, name, comment, resultID, folderID, expiry, subfolderID=)`.
async fn scan_result(c: &Client, call: &Call<'_>) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let system = call.args.first().and_then(|v| v.as_int()).or(c.session.int("solarsystemid2")).unwrap_or(0);
    let target = value_text(call.args.get(3), MAX_SCAN_RESULT_ID);
    let Some(pos) = super::exploration::scan_result_position(&c.db, system, me, &target).await? else {
        return Err(notify_error("请先将信号扫描到 100% 再保存位置。"));
    };
    let folder = call.args.get(4).and_then(|v| v.as_int()).unwrap_or(0);
    add(&c.db, me, folder, subfolder_arg(call, 6), None, Some(TYPE_SOLAR_SYSTEM), system, pos, &value_text(call.args.get(1), MAX_NAME), &value_text(call.args.get(2), MAX_DESCRIPTION), expiry(call, 5)).await
}


pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match (call.service, call.method) {
        ("beyonce", "BookmarkLocation") => return Some(space_location(c, call).await),
        ("beyonce", "BookmarkScanResult") => return Some(scan_result(c, call).await),
        ("accessGroupBookmarkMgr", _) => {}
        _ => return None,
    }
    let me = c.session.int("charid").unwrap_or(0);
    let int = |i: usize| call.args.get(i).and_then(|v| v.as_int()).unwrap_or(0);
    let db = &c.db;
    Some(match call.method {
        // -> (folders, bookmarks, subfolders)
        "GetMyActiveBookmarks" => {
            let folders = match db.query("SELECT folder_id, name, description FROM game.bookmark_folders WHERE owner_id = $1 ORDER BY folder_id", &[&me]).await {
                Ok(r) => r,
                Err(e) => return Some(Err(db_err(e))),
            };
            let now = filetime_now();
            let bookmarks = match db
                .query(
                    &format!("{BOOKMARK_SELECT} WHERE folder_id IN (SELECT folder_id FROM game.bookmark_folders WHERE owner_id = $1) AND (expiry IS NULL OR expiry > $2)"),
                    &[&me, &now],
                )
                .await
            {
                Ok(r) => r,
                Err(e) => return Some(Err(db_err(e))),
            };
            let subfolders = match db
                .query("SELECT subfolder_id, folder_id, name FROM game.bookmark_subfolders WHERE folder_id IN (SELECT folder_id FROM game.bookmark_folders WHERE owner_id = $1)", &[&me])
                .await
            {
                Ok(r) => r,
                Err(e) => return Some(Err(db_err(e))),
            };
            Ok(Value::tuple([
                Value::list(folders.iter().map(|r| folder_row(r, me))),
                Value::list(bookmarks.iter().map(bookmark_row)),
                Value::list(subfolders.iter().map(|r| subfolder_row(r, me))),
            ]))
        }
        // AddFolder(isPersonal, name, description, adminGroupID, manageGroupID, useGroupID, viewGroupID)
        "AddFolder" => match db
            .query_one(
                "INSERT INTO game.bookmark_folders (owner_id, name, description) VALUES ($1, $2, $3) RETURNING folder_id, name, description",
                &[&me, &value_text(call.args.get(1), MAX_NAME), &value_text(call.args.get(2), MAX_DESCRIPTION)],
            )
            .await
        {
            Ok(r) => Ok(folder_row(&r, me)),
            Err(e) => Err(db_err(e)),
        },
        // UpdateFolder(folderID, name, description, ...) -> access level
        "UpdateFolder" => db
            .execute(
                "UPDATE game.bookmark_folders SET name = $3, description = $4 WHERE folder_id = $1 AND owner_id = $2",
                &[&int(0), &me, &value_text(call.args.get(1), MAX_NAME), &value_text(call.args.get(2), MAX_DESCRIPTION)],
            )
            .await
            .map(|_| Value::Int(ACCESS_PERSONAL))
            .map_err(db_err),
        "DeleteFolder" => {
            let folder = int(0);
            db.transaction(
                |tx, _: &mut Outbox| Box::pin(async move {
                    own_folder(tx, me, folder).await?;
                    for sql in ["DELETE FROM game.bookmarks WHERE folder_id = $1", "DELETE FROM game.bookmark_subfolders WHERE folder_id = $1", "DELETE FROM game.bookmark_folders WHERE folder_id = $1"] {
                        Executor::execute(tx, sql, &[&folder]).await.map_err(db_err)?;
                    }
                    Ok(Value::None)
                }),
                |value, _| async move { Ok(value) },
            ).await
        }
        "GetFolderInfo" => match db.query_opt("SELECT folder_id, name, description FROM game.bookmark_folders WHERE folder_id = $1 AND owner_id = $2", &[&int(0), &me]).await {
            Ok(r) => Ok(r.map(|r| folder_row(&r, me)).unwrap_or(Value::None)),
            Err(e) => Err(db_err(e)),
        },
        "AddToKnownFolders" | "UpdateKnownFolderState" => Err(notify_error("暂不支持共享书签文件夹。")),
        "RemoveFromKnownFolders" => Ok(Value::None),
        // CreateSubfolder(folderID, name)
        "CreateSubfolder" => {
            let folder = int(0);
            if let Err(e) = own_folder(db, me, folder).await {
                return Some(Err(e));
            }
            match db
                .query_one("INSERT INTO game.bookmark_subfolders (folder_id, name) VALUES ($1, $2) RETURNING subfolder_id, folder_id, name", &[&folder, &value_text(call.args.get(1), MAX_NAME)])
                .await
            {
                Ok(r) => Ok(subfolder_row(&r, me)),
                Err(e) => Err(db_err(e)),
            }
        }
        // UpdateSubfolder(folderID, subfolderID, name) -> success
        "UpdateSubfolder" => {
            if let Err(e) = own_folder(db, me, int(0)).await {
                return Some(Err(e));
            }
            db
                .execute("UPDATE game.bookmark_subfolders SET name = $3 WHERE subfolder_id = $2 AND folder_id = $1", &[&int(0), &int(1), &value_text(call.args.get(2), MAX_NAME)])
                .await
                .map(|n| Value::Bool(n > 0))
                .map_err(db_err)
        }
        // DeleteSubfolder(folderID, subfolderID) -> the bookmark IDs deleted with it
        "DeleteSubfolder" => {
            let folder = int(0);
            let subfolder = int(1);
            db.transaction(
                |tx, _: &mut Outbox| Box::pin(async move {
                    own_folder(tx, me, folder).await?;
                    let deleted = Executor::query(tx, "DELETE FROM game.bookmarks WHERE folder_id = $1 AND subfolder_id = $2 RETURNING bookmark_id", &[&folder, &subfolder]).await.map_err(db_err)?;
                    Executor::execute(tx, "DELETE FROM game.bookmark_subfolders WHERE subfolder_id = $1 AND folder_id = $2", &[&subfolder, &folder]).await.map_err(db_err)?;
                    Ok(Value::list(deleted.iter().map(|r| Value::Int(r.get(0)))))
                }),
                |value, _| async move { Ok(value) },
            ).await
        }
        "BookmarkStaticLocation" => static_location(c, call).await,
        // UpdateBookmark(bookmarkID, folderID, name, note, subfolderID, newFolderID, expiryCancel)
        "UpdateBookmark" => {
            let new_folder = call.args.get(5).and_then(|v| v.as_int()).unwrap_or(int(1));
            if let Err(e) = own_folder(db, me, new_folder).await {
                return Some(Err(e));
            }
            let cancel = call.args.get(6).is_some_and(|v| matches!(v, Value::Bool(true)) || v.as_int() == Some(1));
            db
                .execute(
                    "UPDATE game.bookmarks SET memo = $3, note = $4, subfolder_id = $5, folder_id = $6, expiry = CASE WHEN $7 THEN NULL ELSE expiry END
                     WHERE bookmark_id = $1 AND folder_id = $2 AND folder_id IN (SELECT folder_id FROM game.bookmark_folders WHERE owner_id = $8)",
                    &[&int(0), &int(1), &value_text(call.args.get(2), MAX_NAME), &value_text(call.args.get(3), MAX_DESCRIPTION), &call.args.get(4).and_then(|v| v.as_int()), &new_folder, &cancel, &me],
                )
                .await
                .map(|_| Value::None)
                .map_err(db_err)
        }
        // DeleteBookmarks(folderID, {bookmarkID}) -> deleted IDs
        "DeleteBookmarks" => {
            let wanted = value_id_list(call.args.get(1));
            match db
                .query(
                    "DELETE FROM game.bookmarks WHERE folder_id = $1 AND bookmark_id = ANY($2)
                     AND folder_id IN (SELECT folder_id FROM game.bookmark_folders WHERE owner_id = $3) RETURNING bookmark_id",
                    &[&int(0), &wanted, &me],
                )
                .await
            {
                Ok(r) => Ok(Value::list(r.iter().map(|r| Value::Int(r.get(0))))),
                Err(e) => Err(db_err(e)),
            }
        }
        // MoveBookmarksToFolderAndSubfolder(oldFolderID, folderID, subfolderID, bookmarkIDs) -> (rows, message)
        "MoveBookmarksToFolderAndSubfolder" => {
            let (to, sub, wanted) = (int(1), call.args.get(2).and_then(|v| v.as_int()), value_id_list(call.args.get(3)));
            if let Err(e) = own_folder(db, me, to).await {
                return Some(Err(e));
            }
            match db
                .query(
                    "UPDATE game.bookmarks SET folder_id = $2, subfolder_id = $3 WHERE folder_id = $1 AND bookmark_id = ANY($4)
                     AND folder_id IN (SELECT folder_id FROM game.bookmark_folders WHERE owner_id = $5) RETURNING bookmark_id, folder_id, subfolder_id",
                    &[&int(0), &to, &sub, &wanted, &me],
                )
                .await
            {
                Ok(rows) => Ok(Value::tuple([
                    Value::list(rows.iter().map(|r| {
                        keyval([
                            ("bookmarkID", Value::Int(r.get(0))),
                            ("folderID", Value::Int(r.get(1))),
                            ("subfolderID", r.get::<_, Option<i64>>(2).map(Value::Int).unwrap_or(Value::None)),
                        ])
                    })),
                    Value::None,
                ])),
                Err(e) => Err(db_err(e)),
            }
        }
        // CopyBookmarksToFolderAndSubfolder(oldFolderID, folderID, subfolderID, bookmarkIDs) -> ({id: bookmark}, message)
        "CopyBookmarksToFolderAndSubfolder" => {
            let (to, sub, wanted) = (int(1), call.args.get(2).and_then(|v| v.as_int()), value_id_list(call.args.get(3)));
            if let Err(e) = own_folder(db, me, to).await {
                return Some(Err(e));
            }
            match db
                .query(
                    "INSERT INTO game.bookmarks (folder_id, subfolder_id, creator_id, item_id, type_id, location_id, x, y, z, memo, note, created, expiry)
                     SELECT $2, $3, $4, item_id, type_id, location_id, x, y, z, memo, note, created, expiry FROM game.bookmarks
                     WHERE folder_id = $1 AND bookmark_id = ANY($5) AND folder_id IN (SELECT folder_id FROM game.bookmark_folders WHERE owner_id = $4)
                     RETURNING bookmark_id, folder_id, subfolder_id, item_id, type_id, location_id, x, y, z, memo, note, created, expiry, creator_id",
                    &[&int(0), &to, &sub, &me, &wanted],
                )
                .await
            {
                Ok(rows) => Ok(Value::tuple([Value::Dict(rows.iter().map(|r| (Value::Int(r.get(0)), bookmark_row(r))).collect()), Value::None])),
                Err(e) => Err(db_err(e)),
            }
        }
        _ => return None,
    })
}
