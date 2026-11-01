//! `accessGroupBookmarkMgr` folder reads and known subscriptions for `bookmarkSvc.py`.
//! Uses bookmark folders and known rows; affiliation changes are checked on each request.
use super::*;


fn unavailable() -> CallError {
    CallError::User { msg: messages::MSG_BOOKMARK_FOLDER_NO_LONGER_THERE.into(), dict: vec![] }
}

async fn contents(ex: &(impl Executor + ?Sized), ids: &[i64]) -> Result<(Value, Value), CallError> {
    let bookmarks = dao::bookmarks(ex, ids, filetime_now()).await.map_err(db_err)?;
    let subfolders = dao::subfolders(ex, ids).await.map_err(db_err)?;
    Ok((Value::list(bookmarks.iter().map(bookmark_row)), Value::list(subfolders.iter().map(subfolder_row))))
}

async fn collection(c: &Client, me: i64, gated: bool) -> Result<Option<Value>, CallError> {
    c.db.transaction(|tx, _: &mut Outbox| Box::pin(async move {
        let known = dao::known::known(tx, me).await.map_err(db_err)?;
        let mut folders = Vec::new();
        let mut active = Vec::new();
        for row in known {
            let Some(folder) = dao::folder(tx, row.folder_id).await.map_err(db_err)? else { continue };
            if !folder.is_personal && !gated { return Ok(None); }
            let tier = access::tier(tx, me, &folder).await?;
            if tier == access::NONE {
                if gated { dao::known::remove_known(tx, me, row.folder_id).await.map_err(db_err)?; }
                continue;
            }
            if row.is_active { active.push(folder.folder_id); }
            folders.push(folder_row(&folder, row.is_active, tier));
        }
        let (bookmarks, subfolders) = contents(tx, &active).await?;
        Ok(Some(Value::tuple([Value::list(folders), bookmarks, subfolders])))
    }), |value, _| async move { Ok(value) }).await
}

async fn get_collection(c: &Client, me: i64) -> CallResult {
    if let Some(value) = collection(c, me, false).await? { return Ok(value); }
    let _guard = crate::services::access_groups::lock().await;
    collection(c, me, true).await?.ok_or_else(unavailable)
}

async fn info(c: &Client, me: i64, id: i64) -> CallResult {
    let folder = dao::folder(&c.db, id).await.map_err(db_err)?.ok_or_else(unavailable)?;
    let _guard = if folder.is_personal { None } else { Some(crate::services::access_groups::lock().await) };
    c.db.transaction(|tx, _: &mut Outbox| Box::pin(async move {
        let Some(folder) = dao::folder(tx, id).await.map_err(db_err)? else { return Ok::<_, CallError>(None) };
        let tier = access::tier(tx, me, &folder).await?;
        if tier == access::NONE {
            if !folder.is_personal { dao::known::remove_known(tx, me, id).await.map_err(db_err)?; }
            return Ok(None);
        }
        let active = dao::known::known(tx, me).await.map_err(db_err)?.iter().any(|row| row.folder_id == id && row.is_active);
        Ok(Some(folder_row(&folder, active, tier)))
    }), |value, _| async move { Ok(value) }).await?.ok_or_else(unavailable)
}

async fn search(c: &Client, me: i64) -> CallResult {
    let _guard = crate::services::access_groups::lock().await;
    c.db.transaction(|tx, _: &mut Outbox| Box::pin(async move {
        let known = dao::known::known(tx, me).await.map_err(db_err)?;
        let mut rows = Vec::new();
        for folder in dao::all_folders(tx).await.map_err(db_err)? {
            if !folder.is_personal {
                let tier = access::tier(tx, me, &folder).await?;
                if tier == access::NONE && known.iter().any(|row| row.folder_id == folder.folder_id) {
                    dao::known::remove_known(tx, me, folder.folder_id).await.map_err(db_err)?;
                } else if tier == access::ADMIN {
                    let active = known.iter().any(|row| row.folder_id == folder.folder_id && row.is_active);
                    rows.push(folder_row(&folder, active, access::ADMIN));
                }
            }
        }
        Ok(Value::list(rows))
    }), |value, _| async move { Ok(value) }).await
}

async fn set_known(c: &Client, me: i64, id: i64, active: bool) -> CallResult {
    let _guard = crate::services::access_groups::lock().await;
    c.db.transaction(|tx, _: &mut Outbox| Box::pin(async move {
        let Some(folder) = dao::folder(tx, id).await.map_err(db_err)? else { return Ok(None) };
        let tier = access::tier(tx, me, &folder).await?;
        if tier == access::NONE {
            dao::known::remove_known(tx, me, id).await.map_err(db_err)?;
            return Ok(None);
        }
        let known = dao::known::known(tx, me).await.map_err(db_err)?;
        let mut known_count = 0;
        let mut already_known = false;
        let mut active_count = 0;
        for row in &known {
            if let Some(other) = dao::folder(tx, row.folder_id).await.map_err(db_err)? {
                if access::tier(tx, me, &other).await? == access::NONE {
                    dao::known::remove_known(tx, me, row.folder_id).await.map_err(db_err)?;
                    continue;
                }
                known_count += 1;
                if row.folder_id == id { already_known = true; }
                else if row.is_active && other.is_personal == folder.is_personal { active_count += 1; }
            }
        }
        if !already_known && known_count >= MAX_KNOWN { return Err(notify_error(messages::bookmarks::BOOKMARK_LIMIT_REACHED)); }
        if active && active_count >= if folder.is_personal { MAX_ACTIVE_PERSONAL } else { MAX_ACTIVE_SHARED } {
            return Err(notify_error(messages::bookmarks::BOOKMARK_LIMIT_REACHED));
        }
        dao::known::set_known(tx, me, id, active).await.map_err(db_err)?;
        let (bookmarks, subfolders) = if active { contents(tx, &[id]).await? } else { (Value::list([]), Value::list([])) };
        Ok(Some(Value::tuple([folder_row(&folder, active, tier), bookmarks, subfolders])))
    }), |value, outbox| async move { crate::db::outbox::dispatch_world(value, outbox, &c.world) }).await?.ok_or_else(unavailable)
}

async fn remove_known(c: &Client, me: i64, id: i64) -> CallResult {
    let _guard = crate::services::access_groups::lock().await;
    c.db.transaction(|tx, _: &mut Outbox| Box::pin(async move {
        dao::known::remove_known(tx, me, id).await.map_err(db_err)?;
        Ok(Value::None)
    }), |value, outbox| async move { crate::db::outbox::dispatch_world(value, outbox, &c.world) }).await
}

async fn static_location(c: &Client, call: &Call<'_>) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let item = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
    let map = &c.world.sde().map;
    let r = map.station(item).and_then(|station| {
        map.celestial(item).map(|celestial| (
            station.solar_system_id.expect("station solar_system_id is NULL"),
            station.type_id.expect("station type_id is NULL") as i32,
            [celestial.x.expect("celestial x is NULL"), celestial.y.expect("celestial y is NULL"), celestial.z.expect("celestial z is NULL")],
        ))
    }).or_else(|| map.system(item).map(|system| (system.solar_system_id, TYPE_SOLAR_SYSTEM, [0.0; 3])));
    // Regions and constellations bookmark the solar system the pilot is in.
    let (system, type_id, pos) = match r {
        Some((system, type_id, pos)) => (system, Some(type_id), pos),
        None => (c.session.int("solarsystemid2").unwrap_or(0), Some(TYPE_SOLAR_SYSTEM), [0.0; 3]),
    };
    let folder = call.args.get(1).and_then(|v| v.as_int()).unwrap_or(0);
    mutations::add(c, me, folder, subfolder_arg(call, 5), Some(item), type_id, system, pos, &value_text(call.args.get(2), MAX_NAME), &value_text(call.args.get(3), MAX_DESCRIPTION), expiry(call, 4)?).await
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let me = c.session.int("charid").unwrap_or(0);
    let id = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
    Some(match call.method {
        "GetMyActiveBookmarks" => get_collection(c, me).await,
        "GetFolderInfo" => info(c, me, id).await,
        "SearchFoldersWithAdminAccess" => search(c, me).await,
        "AddToKnownFolders" | "UpdateKnownFolderState" => {
            let active = call.args.get(1).is_some_and(|v| matches!(v, Value::Bool(true)) || v.as_int() == Some(1));
            set_known(c, me, id, active).await
        }
        "RemoveFromKnownFolders" => remove_known(c, me, id).await,
        "BookmarkStaticLocation" => static_location(c, call).await,
        _ => return mutations::dispatch(c, call).await,
    })
}
