//! Post-commit bookmark folder notifications for `bookmarkSvc.py`.
//! Reads known-folder subscriptions and effective access from the bookmark and group tables.
//! Delivery to offline characters is naturally dropped by the world.
use super::{access, dao, db_err, CallError, Executor, Outbox, Value};

pub(crate) struct AccessSnapshot {
    folder: i64,
    character: i64,
    tier: i64,
    known: bool,
}

pub(super) async fn snapshot_folder(ex: &(impl Executor + ?Sized), folder: &dao::Folder, online: &[i64]) -> Result<Vec<AccessSnapshot>, CallError> {
    let known = dao::known::known_for_folder(ex, folder.folder_id).await.map_err(db_err)?;
    let mut characters: Vec<i64> = known.iter().map(|row| row.character_id).chain(online.iter().copied()).collect();
    characters.sort_unstable();
    characters.dedup();
    let mut snapshots = Vec::with_capacity(characters.len());
    for character in characters {
        snapshots.push(AccessSnapshot { folder: folder.folder_id, character, tier: access::tier(ex, character, folder).await?, known: known.iter().any(|row| row.character_id == character) });
    }
    Ok(snapshots)
}

pub(crate) async fn snapshot_group(ex: &(impl Executor + ?Sized), group: i64, online: &[i64]) -> Result<Vec<AccessSnapshot>, CallError> {
    let mut snapshots = Vec::new();
    for folder in dao::folders_using_group(ex, group).await.map_err(db_err)? {
        snapshots.extend(snapshot_folder(ex, &folder, online).await?);
    }
    Ok(snapshots)
}

pub(crate) async fn access_changes(ex: &(impl Executor + ?Sized), outbox: &mut Outbox, before: &[AccessSnapshot]) -> Result<(), CallError> {
    for old in before {
        if !old.known || old.tier == access::NONE { continue; }
        let new = access::get(ex, old.character, old.folder).await?.map_or(access::NONE, |(_, tier)| tier);
        if new == access::NONE {
            dao::known::remove_known(ex, old.character, old.folder).await.map_err(db_err)?;
            outbox.notify_pilot(old.character, "OnSharedBookmarksFoldersAccessLost", vec![Value::list([Value::Int(old.folder)])]);
        } else if new != old.tier {
            outbox.notify_pilot(old.character, "OnSharedBookmarksFoldersAccessChanged", vec![Value::Dict(vec![(Value::Int(old.folder), Value::Int(new))])]);
        }
    }
    Ok(())
}

pub(super) async fn audience(ex: &(impl Executor + ?Sized), folder: &dao::Folder, active_only: bool) -> Result<Vec<i64>, CallError> {
    let mut result = Vec::new();
    for row in dao::known::known_for_folder(ex, folder.folder_id).await.map_err(db_err)? {
        if (!active_only || row.is_active) && access::tier(ex, row.character_id, folder).await? != access::NONE {
            result.push(row.character_id);
        }
    }
    Ok(result)
}

pub(super) fn updated(outbox: &mut Outbox, audience: &[i64], folder: i64, kind: &str, args: Value) {
    let update = Value::tuple([Value::tuple([Value::Int(folder)]), Value::str(kind), args]);
    for character in audience {
        outbox.notify_pilot(*character, "OnSharedBookmarksFolderUpdated", vec![Value::list([update.clone()])]);
    }
}
