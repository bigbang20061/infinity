//! After-commit bookmark notifications and revocation.
use super::*;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn subscribed_receiver_is_evicted_before_later_updates() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let guest = fixture::create(&db).await.unwrap();
    let outsider = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut a, _a) = crate::services::tests::client(test_db.clone(), owner.character).await;
        let (mut b, _b) = crate::services::tests::client(test_db.clone(), guest.character).await;
        b.world = a.world.clone();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (outsider_tx, mut outsider_rx) = tokio::sync::mpsc::unbounded_channel();
        a.world.register_pilot(guest.character, tx);
        a.world.register_pilot(outsider.character, outsider_tx);
        let admin = group(&mut a, "admin").await;
        let view = group(&mut a, "view").await;
        let use_group = group(&mut a, "use").await;
        grant(&mut a, view, guest.character).await;
        let f = folder(&mut a, [Some(admin), None, Some(use_group), Some(view)]).await;
        rpc(&mut b, "accessGroupBookmarkMgr", "AddToKnownFolders", &[Value::Int(f), Value::Bool(true)]).await.unwrap();
        while rx.try_recv().is_ok() {}
        let added = mutations::add(&a, owner.character, f, None, None, None, 1, [1.0, 2.0, 3.0], "place", "note", None).await.unwrap();
        let id = added.items().unwrap()[0].as_int().unwrap();
        let (event, args) = rx.try_recv().unwrap();
        assert_eq!(event, "OnSharedBookmarksFolderUpdated");
        let update = &args[0].items().unwrap()[0];
        let fields = update.items().unwrap();
        assert_eq!(fields[0], Value::tuple([Value::Int(f)]));
        assert_eq!(fields[1], Value::str("bookmarksAdded"));
        assert_eq!(crate::services::value_field(&fields[2].items().unwrap()[0], "bookmarkID"), Some(&Value::Int(id)));
        assert!(outsider_rx.try_recv().is_err());
        grant(&mut a, use_group, guest.character).await;
        let notices: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(notices.iter().any(|(event, args)| event == "OnSharedBookmarksFoldersAccessChanged" && args == &vec![Value::Dict(vec![(Value::Int(f), Value::Int(access::USE))])]));
        rpc(&mut a, "ownerGroupManager", "DeleteMembers", &[Value::Int(use_group), ids(&[guest.character])]).await.unwrap();
        let notices: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(notices.iter().any(|(event, args)| event == "OnSharedBookmarksFoldersAccessChanged" && args == &vec![Value::Dict(vec![(Value::Int(f), Value::Int(access::VIEW))])]));
        let remove = rpc(&mut a, "ownerGroupManager", "DeleteMembers", &[Value::Int(view), ids(&[guest.character])]).await.unwrap();
        assert_eq!(remove, Value::list([]));
        let notices: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(notices.iter().any(|(event, args)| event == "OnSharedBookmarksFoldersAccessLost" && args == &vec![ids(&[f])]));
        assert!(!dao::known::known(&test_db, guest.character).await.unwrap().iter().any(|k| k.folder_id == f));
        mutations::add(&a, owner.character, f, None, None, None, 1, [4.0, 5.0, 6.0], "later", "", None).await.unwrap();
        assert!(rx.try_recv().is_err());
        assert!(outsider_rx.try_recv().is_err());
        grant(&mut a, view, guest.character).await;
        assert!(!dao::known::known(&test_db, guest.character).await.unwrap().iter().any(|k| k.folder_id == f));
        rpc(&mut b, "accessGroupBookmarkMgr", "AddToKnownFolders", &[Value::Int(f), Value::Bool(true)]).await.unwrap();
        while rx.try_recv().is_ok() {}
        rpc(&mut a, "ownerGroupManager", "Delete", &[Value::Int(view)]).await.unwrap();
        let notices: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(notices.iter().any(|(event, args)| event == "OnSharedBookmarksFoldersAccessLost" && args == &vec![ids(&[f])]));
        assert!(dao::folder(&test_db, f).await.unwrap().unwrap().view_group_id.is_none());
        assert_eq!(dao::bookmarks_by_ids(&test_db, f, &[id]).await.unwrap().len(), 1);
    }).await;
    cleanup(&db, &[owner.character, guest.character, outsider.character]).await;
    for f in [owner, guest, outsider] { fixture::cleanup(&db, f).await.unwrap(); }
    result.unwrap();
}

fn update_payload(event: (String, Vec<Value>), kind: &str, folder: i64) -> Value {
    assert_eq!(event.0, "OnSharedBookmarksFolderUpdated");
    let entries = event.1[0].items().unwrap();
    assert_eq!(entries.len(), 1);
    let fields = entries[0].items().unwrap();
    assert_eq!(fields[0], Value::tuple([Value::Int(folder)]));
    assert_eq!(fields[1], Value::str(kind));
    fields[2].clone()
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn mutation_payloads_partition_moves_and_refusals() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let source = fixture::create(&db).await.unwrap();
    let destination = fixture::create(&db).await.unwrap();
    let both = fixture::create(&db).await.unwrap();
    let outsider = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut a, _a) = crate::services::tests::client(test_db.clone(), owner.character).await;
        let (mut s, _s) = crate::services::tests::client(test_db.clone(), source.character).await;
        let (mut d, _d) = crate::services::tests::client(test_db.clone(), destination.character).await;
        let (mut b, _b) = crate::services::tests::client(test_db.clone(), both.character).await;
        s.world = a.world.clone(); d.world = a.world.clone(); b.world = a.world.clone();
        let (stx, mut sr) = tokio::sync::mpsc::unbounded_channel();
        let (dtx, mut dr) = tokio::sync::mpsc::unbounded_channel();
        let (btx, mut br) = tokio::sync::mpsc::unbounded_channel();
        let (otx, mut or) = tokio::sync::mpsc::unbounded_channel();
        for (character, tx) in [(source.character, stx), (destination.character, dtx), (both.character, btx), (outsider.character, otx)] { a.world.register_pilot(character, tx); }
        let admin = group(&mut a, "admin").await;
        let sg = group(&mut a, "source").await;
        let dg = group(&mut a, "destination").await;
        for id in [source.character, both.character] { grant(&mut a, sg, id).await; }
        for id in [destination.character, both.character] { grant(&mut a, dg, id).await; }
        let from = folder(&mut a, [Some(admin), None, None, Some(sg)]).await;
        let to = folder(&mut a, [Some(admin), None, None, Some(dg)]).await;
        rpc(&mut s, "accessGroupBookmarkMgr", "AddToKnownFolders", &[Value::Int(from), Value::Bool(true)]).await.unwrap();
        rpc(&mut d, "accessGroupBookmarkMgr", "AddToKnownFolders", &[Value::Int(to), Value::Bool(true)]).await.unwrap();
        rpc(&mut b, "accessGroupBookmarkMgr", "AddToKnownFolders", &[Value::Int(from), Value::Bool(true)]).await.unwrap();
        rpc(&mut b, "accessGroupBookmarkMgr", "AddToKnownFolders", &[Value::Int(to), Value::Bool(true)]).await.unwrap();
        while sr.try_recv().is_ok() {} while dr.try_recv().is_ok() {} while br.try_recv().is_ok() {} while or.try_recv().is_ok() {}
        rpc(&mut a, "accessGroupBookmarkMgr", "UpdateFolder", &[Value::Int(from), Value::unicode("renamed"), Value::unicode("description"), Value::Int(admin), Value::None, Value::None, Value::Int(sg)]).await.unwrap();
        assert_eq!(update_payload(sr.try_recv().unwrap(), "folderUpdated", from), Value::tuple([Value::unicode("renamed"), Value::unicode("description")]));
        update_payload(br.try_recv().unwrap(), "folderUpdated", from);
        let foreign = mutations::add(&a, owner.character, to, None, None, None, 1, [0.0; 3], "destination", "", None).await.unwrap().items().unwrap()[0].as_int().unwrap();
        update_payload(dr.try_recv().unwrap(), "bookmarksAdded", to);
        update_payload(br.try_recv().unwrap(), "bookmarksAdded", to);
        let subrow = rpc(&mut a, "accessGroupBookmarkMgr", "CreateSubfolder", &[Value::Int(from), Value::unicode("one")]).await.unwrap();
        let sub = crate::services::value_field(&subrow, "subfolderID").unwrap().as_int().unwrap();
        assert_eq!(update_payload(sr.try_recv().unwrap(), "subfolderAdded", from), subrow);
        update_payload(br.try_recv().unwrap(), "subfolderAdded", from);
        rpc(&mut a, "accessGroupBookmarkMgr", "UpdateSubfolder", &[Value::Int(from), Value::Int(sub), Value::unicode("two")]).await.unwrap();
        assert_eq!(update_payload(sr.try_recv().unwrap(), "subfolderUpdated", from), Value::tuple([Value::Int(sub), Value::unicode("two")]));
        update_payload(br.try_recv().unwrap(), "subfolderUpdated", from);
        let added = mutations::add(&a, owner.character, from, Some(sub), None, None, 1, [1.0, 2.0, 3.0], "place", "note", None).await.unwrap();
        let id = added.items().unwrap()[0].as_int().unwrap();
        update_payload(sr.try_recv().unwrap(), "bookmarksAdded", from);
        update_payload(br.try_recv().unwrap(), "bookmarksAdded", from);
        rpc(&mut a, "accessGroupBookmarkMgr", "UpdateBookmark", &[Value::Int(id), Value::Int(from), Value::unicode("changed"), Value::unicode("note"), Value::Int(sub), Value::Int(from), Value::Bool(false)]).await.unwrap();
        let changed = update_payload(sr.try_recv().unwrap(), "bookmarksUpdated", from);
        assert_eq!(crate::services::value_field(&changed.items().unwrap()[0], "memo"), Some(&Value::unicode("changed")));
        update_payload(br.try_recv().unwrap(), "bookmarksUpdated", from);
        for method in ["MoveBookmarksToFolderAndSubfolder", "CopyBookmarksToFolderAndSubfolder"] {
            assert!(rpc(&mut a, "accessGroupBookmarkMgr", method, &[Value::Int(from), Value::Int(to), Value::Int(sub), ids(&[id])]).await.is_err());
            assert!(rpc(&mut a, "accessGroupBookmarkMgr", method, &[Value::Int(from), Value::Int(to), Value::None, ids(&[id, i64::MAX])]).await.is_err());
            assert!(rpc(&mut a, "accessGroupBookmarkMgr", method, &[Value::Int(from), Value::Int(to), Value::None, ids(&[id, foreign])]).await.is_err());
        }
        assert_eq!(dao::bookmarks_by_ids(&test_db, from, &[id]).await.unwrap().len(), 1);
        assert_eq!(dao::bookmarks(&test_db, &[to], i64::MIN).await.unwrap().len(), 1);
        assert!(sr.try_recv().is_err() && dr.try_recv().is_err() && br.try_recv().is_err() && or.try_recv().is_err());
        rpc(&mut a, "accessGroupBookmarkMgr", "MoveBookmarksToFolderAndSubfolder", &[Value::Int(from), Value::Int(to), Value::None, ids(&[id])]).await.unwrap();
        assert_eq!(update_payload(sr.try_recv().unwrap(), "bookmarksRemoved", from), ids(&[id]));
        let added = update_payload(dr.try_recv().unwrap(), "bookmarksAdded", to);
        let row = &added.items().unwrap()[0];
        assert_eq!(crate::services::value_field(row, "bookmarkID"), Some(&Value::Int(id)));
        assert_eq!(crate::services::value_field(row, "folderID"), Some(&Value::Int(to)));
        assert_eq!(crate::services::value_field(row, "memo"), Some(&Value::unicode("changed")));
        let moved = update_payload(br.try_recv().unwrap(), "bookmarksMoved", from);
        assert_eq!(moved, Value::tuple([added, Value::Int(from), Value::Int(to)]));
        assert!(or.try_recv().is_err());
        rpc(&mut a, "accessGroupBookmarkMgr", "DeleteSubfolder", &[Value::Int(from), Value::Int(sub)]).await.unwrap();
        assert_eq!(update_payload(sr.try_recv().unwrap(), "bookmarksRemoved", from), ids(&[]));
        assert_eq!(update_payload(sr.try_recv().unwrap(), "subfolderRemoved", from), Value::Int(sub));
        update_payload(br.try_recv().unwrap(), "bookmarksRemoved", from);
        update_payload(br.try_recv().unwrap(), "subfolderRemoved", from);
        rpc(&mut a, "accessGroupBookmarkMgr", "DeleteBookmarks", &[Value::Int(to), ids(&[id])]).await.unwrap();
        assert_eq!(update_payload(dr.try_recv().unwrap(), "bookmarksRemoved", to), ids(&[id]));
        assert_eq!(update_payload(br.try_recv().unwrap(), "bookmarksRemoved", to), ids(&[id]));
        rpc(&mut a, "accessGroupBookmarkMgr", "DeleteFolder", &[Value::Int(from)]).await.unwrap();
        assert_eq!(sr.try_recv().unwrap(), ("OnSharedBookmarksFolderDeleted".to_string(), vec![Value::Int(from)]));
        assert_eq!(br.try_recv().unwrap(), ("OnSharedBookmarksFolderDeleted".to_string(), vec![Value::Int(from)]));
        assert!(or.try_recv().is_err());
    }).await;
    cleanup(&db, &[owner.character, source.character, destination.character, both.character, outsider.character]).await;
    for f in [owner, source, destination, both, outsider] { fixture::cleanup(&db, f).await.unwrap(); }
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn shared_gate_orders_update_and_revocation_in_both_directions() {
    use std::{future::{poll_fn, Future}, task::Poll};
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let guest = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut a, _a) = crate::services::tests::client(test_db.clone(), owner.character).await;
        let (mut a2, _a2) = crate::services::tests::client(test_db.clone(), owner.character).await;
        let (mut b, _b) = crate::services::tests::client(test_db.clone(), guest.character).await;
        a2.world = a.world.clone(); b.world = a.world.clone();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        a.world.register_pilot(guest.character, tx);
        let admin = group(&mut a, "admin").await;
        let view = group(&mut a, "view").await;
        grant(&mut a, view, guest.character).await;
        let f = folder(&mut a, [Some(admin), None, None, Some(view)]).await;
        rpc(&mut b, "accessGroupBookmarkMgr", "AddToKnownFolders", &[Value::Int(f), Value::Bool(true)]).await.unwrap();
        while rx.try_recv().is_ok() {}
        {
        let guard = crate::services::access_groups::lock().await;
        let add = mutations::add(&a, owner.character, f, None, None, None, 1, [0.0; 3], "before", "", None);
        let args = [Value::Int(view), ids(&[guest.character])];
        let revoke = rpc(&mut a2, "ownerGroupManager", "DeleteMembers", &args);
        tokio::pin!(add, revoke);
        poll_fn(|cx| { assert!(add.as_mut().poll(cx).is_pending()); assert!(revoke.as_mut().poll(cx).is_pending()); Poll::Ready(()) }).await;
        drop(guard);
        let (add_result, revoke_result) = tokio::join!(add, revoke);
        add_result.unwrap(); revoke_result.unwrap();
        }
        let notices: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).filter(|(event, _)| event.starts_with("OnSharedBookmarks")).collect();
        assert_eq!(notices.len(), 2);
        assert_eq!(notices[0].0, "OnSharedBookmarksFolderUpdated");
        assert_eq!(notices[1], ("OnSharedBookmarksFoldersAccessLost".to_string(), vec![ids(&[f])]));
        grant(&mut a, view, guest.character).await;
        rpc(&mut b, "accessGroupBookmarkMgr", "AddToKnownFolders", &[Value::Int(f), Value::Bool(true)]).await.unwrap();
        while rx.try_recv().is_ok() {}
        {
        let guard = crate::services::access_groups::lock().await;
        let args = [Value::Int(view), ids(&[guest.character])];
        let revoke = rpc(&mut a2, "ownerGroupManager", "DeleteMembers", &args);
        let add = mutations::add(&a, owner.character, f, None, None, None, 1, [0.0; 3], "after", "", None);
        tokio::pin!(revoke, add);
        poll_fn(|cx| { assert!(revoke.as_mut().poll(cx).is_pending()); assert!(add.as_mut().poll(cx).is_pending()); Poll::Ready(()) }).await;
        drop(guard);
        let (revoke_result, add_result) = tokio::join!(revoke, add);
        revoke_result.unwrap(); add_result.unwrap();
        }
        let notices: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).filter(|(event, _)| event.starts_with("OnSharedBookmarks")).collect();
        assert_eq!(notices, vec![("OnSharedBookmarksFoldersAccessLost".to_string(), vec![ids(&[f])])]);
    }).await;
    cleanup(&db, &[owner.character, guest.character]).await;
    for f in [owner, guest] { fixture::cleanup(&db, f).await.unwrap(); }
    result.unwrap();
}
