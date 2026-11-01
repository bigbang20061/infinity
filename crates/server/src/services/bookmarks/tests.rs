//! Shared bookmark wire and authorization regressions.
mod permissions;
mod notifications;

use super::*;
use crate::{client::Client, db::Db, services::{Call, CallError}};
use crate::services::tests::fixture;

async fn rpc(c: &mut Client, service: &str, method: &str, args: &[Value]) -> Result<Value, CallError> {
    let kwargs = Value::Dict(vec![]);
    crate::services::dispatch(c, &Call { service, method, args, kwargs: &kwargs, bound: false, bind_params: None }).await
}
async fn group(c: &mut Client, name: &str) -> i64 {
    rpc(c, "ownerGroupManager", "Create", &[Value::unicode(name), Value::unicode("")]).await.unwrap().as_int().unwrap()
}
async fn grant(c: &mut Client, group: i64, member: i64) {
    let rows = Value::Dict(vec![(Value::Int(member), Value::Int(0))]);
    assert_eq!(rpc(c, "ownerGroupManager", "AddMembers", &[Value::Int(group), rows]).await.unwrap(), Value::list([]));
}
async fn folder(c: &mut Client, groups: [Option<i64>; 4]) -> i64 {
    let mut args = vec![Value::Bool(false), Value::unicode("shared"), Value::unicode("description")];
    args.extend(groups.into_iter().map(|id| id.map(Value::Int).unwrap_or(Value::None)));
    let row = rpc(c, "accessGroupBookmarkMgr", "AddFolder", &args).await.unwrap();
    crate::services::value_field(&row, "folderID").unwrap().as_int().unwrap()
}
fn ids(ids: &[i64]) -> Value { Value::list(ids.iter().copied().map(Value::Int)) }
async fn cleanup(db: &Db, chars: &[i64]) {
    db.execute("DELETE FROM game.bookmarks WHERE folder_id IN (SELECT folder_id FROM game.bookmark_folders WHERE owner_id = ANY($1))", &[&chars]).await.unwrap();
    db.execute("DELETE FROM game.bookmark_subfolders WHERE folder_id IN (SELECT folder_id FROM game.bookmark_folders WHERE owner_id = ANY($1))", &[&chars]).await.unwrap();
    db.execute("DELETE FROM game.bookmark_folders WHERE owner_id = ANY($1)", &[&chars]).await.unwrap();
    db.execute("DELETE FROM game.access_group_logs WHERE character_id = ANY($1)", &[&chars]).await.unwrap();
    db.execute("DELETE FROM game.access_groups WHERE creator_id = ANY($1)", &[&chars]).await.unwrap();
}

#[test]
fn stored_creator_and_unicode_wire_rows_roundtrip() {
    let folder = dao::Folder { folder_id: 10, owner_id: 20, name: "目录".into(), description: "描述".into(), is_personal: false, admin_group_id: Some(30), manage_group_id: Some(31), use_group_id: Some(32), view_group_id: Some(33) };
    let sub = dao::Subfolder { subfolder_id: 40, folder_id: 10, creator_id: 21, name: "子目录".into() };
    let bookmark = dao::Bookmark { bookmark_id: 50, folder_id: 10, subfolder_id: Some(40), item_id: Some(60), type_id: Some(70), location_id: 80, x: 1.5, y: 2.5, z: 3.5, memo: "位置".into(), note: "笔记".into(), created: 90, expiry: Some(100), creator_id: 22 };
    let f = folder_row(&folder, false, access::VIEW);
    assert_eq!(crate::services::value_field(&f, "creatorID"), Some(&Value::Int(20)));
    assert_eq!(crate::services::value_field(&f, "ownerID"), Some(&Value::Int(20)));
    assert_eq!(crate::services::value_field(&f, "isPersonal"), Some(&Value::Bool(false)));
    assert_eq!(crate::services::value_field(&f, "isActive"), Some(&Value::Bool(false)));
    for (key, id) in [("adminGroupID", 30), ("manageGroupID", 31), ("useGroupID", 32), ("viewGroupID", 33)] { assert_eq!(crate::services::value_field(&f, key), Some(&Value::Int(id))); }
    assert_eq!(crate::services::value_field(&f, "folderName"), Some(&Value::unicode("目录")));
    assert_eq!(crate::services::value_field(&f, "folderID"), Some(&Value::Int(10)));
    assert_eq!(crate::services::value_field(&f, "description"), Some(&Value::unicode("描述")));
    assert_eq!(crate::services::value_field(&f, "accessLevel"), Some(&Value::Int(access::VIEW)));
    let personal = folder_row(&dao::Folder { is_personal: true, ..folder.clone() }, true, access::PERSONAL);
    assert_eq!(crate::services::value_field(&personal, "accessLevel"), Some(&Value::Int(1)));
    let s = subfolder_row(&sub);
    assert_eq!(crate::services::value_field(&s, "creatorID"), Some(&Value::Int(21)));
    assert_eq!(crate::services::value_field(&s, "subfolderName"), Some(&Value::unicode("子目录")));
    assert_eq!(crate::services::value_field(&s, "subfolderID"), Some(&Value::Int(40)));
    assert_eq!(crate::services::value_field(&s, "folderID"), Some(&Value::Int(10)));
    let b = bookmark_row(&bookmark);
    assert_eq!(crate::services::value_field(&b, "creatorID"), Some(&Value::Int(22)));
    assert_eq!(crate::services::value_field(&b, "subfolderID"), Some(&Value::Int(40)));
    assert_eq!(crate::services::value_field(&b, "memo"), Some(&Value::unicode("位置")));
    assert_eq!(crate::services::value_field(&b, "note"), Some(&Value::unicode("笔记")));
    assert_eq!(crate::services::value_field(&b, "x"), Some(&Value::Float(1.5)));
    for (key, value) in [
        ("bookmarkID", Value::Int(50)), ("folderID", Value::Int(10)),
        ("itemID", Value::Int(60)), ("typeID", Value::Int(70)),
        ("flag", Value::None), ("locationID", Value::Int(80)),
        ("y", Value::Float(2.5)), ("z", Value::Float(3.5)),
        ("created", Value::Int(90)), ("expiry", Value::Int(100)),
    ] { assert_eq!(crate::services::value_field(&b, key), Some(&value)); }
    for row in [f, s, b] { assert_eq!(marshal::loads(&marshal::dumps(&row)).unwrap(), row); }
}
