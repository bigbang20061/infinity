//! accessGroupBookmarkMgr for `eve/client/script/ui/shared/bookmarks/bookmarkSvc.py`.
//! Uses personal bookmark tables and SDE locations; shared access-group folders are unsupported.
use super::*;

/// `BookmarkStaticLocation(itemID, folderID, name, comment, expiry, subfolderID=)`: a region,
/// constellation, solar system or station.
async fn static_location(c: &Client, call: &Call<'_>) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let item = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
    let map = &c.world.sde().map;
    let r = map.station(item).and_then(|station| {
        map.celestial(item).map(|celestial| (
            station.solar_system_id.expect("station solar_system_id is NULL"),
            station.type_id.expect("station type_id is NULL") as i32,
            [
                celestial.x.expect("celestial x is NULL"),
                celestial.y.expect("celestial y is NULL"),
                celestial.z.expect("celestial z is NULL"),
            ],
        ))
    }).or_else(|| map.system(item).map(|system| (system.solar_system_id, TYPE_SOLAR_SYSTEM, [0.0; 3])));
    // Regions and constellations bookmark the solar system the pilot is in.
    let (system, type_id, pos) = match r {
        Some((system, type_id, pos)) => (system, Some(type_id), pos),
        None => (c.session.int("solarsystemid2").unwrap_or(0), Some(TYPE_SOLAR_SYSTEM), [0.0; 3]),
    };
    let folder = call.args.get(1).and_then(|v| v.as_int()).unwrap_or(0);
    add(&c.db, me, folder, subfolder_arg(call, 5), Some(item), type_id, system, pos, &value_text(call.args.get(2), MAX_NAME), &value_text(call.args.get(3), MAX_DESCRIPTION), expiry(call, 4)).await
}
pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let me = c.session.int("charid").unwrap_or(0);
    let int = |i: usize| call.args.get(i).and_then(|v| v.as_int()).unwrap_or(0);
    let db = &c.db;
    Some(match call.method {
        // -> (folders, bookmarks, subfolders)
        "GetMyActiveBookmarks" => {
            let folders = match dao::folders(db, me).await {
                Ok(r) => r,
                Err(e) => return Some(Err(db_err(e))),
            };
            let now = filetime_now();
            let bookmarks = match dao::bookmarks(db, me, now).await {
                Ok(r) => r,
                Err(e) => return Some(Err(db_err(e))),
            };
            let subfolders = match dao::subfolders(db, me).await {
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
        "AddFolder" => match dao::add_folder(db, me, &value_text(call.args.get(1), MAX_NAME), &value_text(call.args.get(2), MAX_DESCRIPTION)).await {
            Ok(r) => Ok(folder_row(&r, me)),
            Err(e) => Err(db_err(e)),
        },
        // UpdateFolder(folderID, name, description, ...) -> access level
        "UpdateFolder" => dao::update_folder(db, int(0), me, &value_text(call.args.get(1), MAX_NAME), &value_text(call.args.get(2), MAX_DESCRIPTION)).await
            .map(|_| Value::Int(ACCESS_PERSONAL))
            .map_err(db_err),
        "DeleteFolder" => {
            let folder = int(0);
            db.transaction(
                |tx, _: &mut Outbox| Box::pin(async move {
                    own_folder(tx, me, folder).await?;
                    dao::delete_folder(tx, folder).await.map_err(db_err)?;
                    Ok(Value::None)
                }),
                |value, _| async move { Ok(value) },
            ).await
        }
        "GetFolderInfo" => match dao::folder(db, int(0), me).await {
            Ok(r) => Ok(r.map(|r| folder_row(&r, me)).unwrap_or(Value::None)),
            Err(e) => Err(db_err(e)),
        },
        // CreateSubfolder(folderID, name)
        "CreateSubfolder" => {
            let folder = int(0);
            if let Err(e) = own_folder(db, me, folder).await {
                return Some(Err(e));
            }
            match dao::add_subfolder(db, folder, &value_text(call.args.get(1), MAX_NAME)).await {
                Ok(r) => Ok(subfolder_row(&r, me)),
                Err(e) => Err(db_err(e)),
            }
        }
        // UpdateSubfolder(folderID, subfolderID, name) -> success
        "UpdateSubfolder" => {
            if let Err(e) = own_folder(db, me, int(0)).await {
                return Some(Err(e));
            }
            dao::update_subfolder(db, int(0), int(1), &value_text(call.args.get(2), MAX_NAME)).await
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
                    let deleted = dao::delete_subfolder_bookmarks(tx, folder, subfolder).await.map_err(db_err)?;
                    dao::delete_subfolder(tx, folder, subfolder).await.map_err(db_err)?;
                    Ok(Value::list(deleted.iter().map(|&id| Value::Int(id))))
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
            dao::update_bookmark(db, &dao::UpdateBookmark { id: int(0), folder: int(1), memo: &value_text(call.args.get(2), MAX_NAME), note: &value_text(call.args.get(3), MAX_DESCRIPTION), subfolder: call.args.get(4).and_then(|v| v.as_int()), new_folder, cancel, me }).await
                .map(|_| Value::None)
                .map_err(db_err)
        }
        // DeleteBookmarks(folderID, {bookmarkID}) -> deleted IDs
        "DeleteBookmarks" => {
            let wanted = value_id_list(call.args.get(1));
            match dao::delete_bookmarks(db, int(0), &wanted, me).await {
                Ok(r) => Ok(Value::list(r.iter().map(|&id| Value::Int(id)))),
                Err(e) => Err(db_err(e)),
            }
        }
        // MoveBookmarksToFolderAndSubfolder(oldFolderID, folderID, subfolderID, bookmarkIDs) -> (rows, message)
        "MoveBookmarksToFolderAndSubfolder" => {
            let (to, sub, wanted) = (int(1), call.args.get(2).and_then(|v| v.as_int()), value_id_list(call.args.get(3)));
            if let Err(e) = own_folder(db, me, to).await {
                return Some(Err(e));
            }
            match dao::move_bookmarks(db, int(0), to, sub, &wanted, me).await {
                Ok(rows) => Ok(Value::tuple([
                    Value::list(rows.iter().map(|r| {
                        keyval([
                            ("bookmarkID", Value::Int(r.bookmark_id)),
                            ("folderID", Value::Int(r.folder_id)),
                            ("subfolderID", r.subfolder_id.map(Value::Int).unwrap_or(Value::None)),
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
            match dao::copy_bookmarks(db, int(0), to, sub, me, &wanted).await {
                Ok(rows) => Ok(Value::tuple([Value::Dict(rows.iter().map(|r| (Value::Int(r.bookmark_id), bookmark_row(r))).collect()), Value::None])),
                Err(e) => Err(db_err(e)),
            }
        }
        _ => return None,
    })
}
