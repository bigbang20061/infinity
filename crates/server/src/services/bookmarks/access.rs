//! Effective bookmark folder access for `bookmarkSvc.py`.
//! Reads bookmark folders and access-group memberships; affiliation changes take effect on the next check.
use super::{dao, db_err, CallError, Executor};

/// `appConst.ACCESS_NONE`.
pub(super) const NONE: i64 = 0;
/// `appConst.ACCESS_PERSONAL`.
pub(super) const PERSONAL: i64 = 1;
/// `appConst.ACCESS_VIEW`.
pub(super) const VIEW: i64 = 2;
/// `appConst.ACCESS_USE`.
pub(super) const USE: i64 = 3;
/// `appConst.ACCESS_MANAGE`.
pub(super) const MANAGE: i64 = 4;
/// `appConst.ACCESS_ADMIN`.
pub(super) const ADMIN: i64 = 5;

pub(super) async fn tier(ex: &(impl Executor + ?Sized), character: i64, folder: &dao::Folder) -> Result<i64, CallError> {
    if folder.is_personal {
        return Ok(if folder.owner_id == character { PERSONAL } else { NONE });
    }
    for (group, level) in [
        (folder.admin_group_id, ADMIN), (folder.manage_group_id, MANAGE),
        (folder.use_group_id, USE), (folder.view_group_id, VIEW),
    ] {
        if let Some(group) = group {
            let membership = crate::services::access_groups::membership(ex, character, group).await?;
            if (crate::services::access_groups::MEMBER..=crate::services::access_groups::ADMIN).contains(&membership) { return Ok(level); }
        }
    }
    Ok(NONE)
}

pub(super) async fn get(ex: &(impl Executor + ?Sized), character: i64, id: i64) -> Result<Option<(dao::Folder, i64)>, CallError> {
    let Some(folder) = dao::folder(ex, id).await.map_err(db_err)? else { return Ok(None) };
    let level = tier(ex, character, &folder).await?;
    Ok((level != NONE).then_some((folder, level)))
}
