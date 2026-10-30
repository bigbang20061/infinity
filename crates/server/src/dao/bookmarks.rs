//! Bookmark persistence for `eve/client/script/ui/shared/bookmarks/bookmarkSvc.py`.
//! Uses `game.bookmarks`, `game.bookmark_folders`, and `game.bookmark_subfolders`.
//! Shared access-group folders are not modelled.
use crate::db::{executor::Executor, Error};
use tokio_postgres::Row;

pub(crate) struct Folder {
    pub folder_id: i64,
    pub name: String,
    pub description: String,
}
impl Folder {
    fn from_row(r: &Row) -> Self {
        Self { folder_id: r.get("folder_id"), name: r.get("name"), description: r.get("description") }
    }
}
pub(crate) struct Subfolder {
    pub subfolder_id: i64,
    pub folder_id: i64,
    pub name: String,
}
impl Subfolder {
    fn from_row(r: &Row) -> Self {
        Self { subfolder_id: r.get("subfolder_id"), folder_id: r.get("folder_id"), name: r.get("name") }
    }
}
pub(crate) struct Bookmark {
    pub bookmark_id: i64,
    pub folder_id: i64,
    pub subfolder_id: Option<i64>,
    pub item_id: Option<i64>,
    pub type_id: Option<i32>,
    pub location_id: i64,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub memo: String,
    pub note: String,
    pub created: i64,
    pub expiry: Option<i64>,
    pub creator_id: i64,
}
impl Bookmark {
    fn from_row(r: &Row) -> Self {
        Self {
            bookmark_id: r.get("bookmark_id"),
            folder_id: r.get("folder_id"),
            subfolder_id: r.get("subfolder_id"),
            item_id: r.get("item_id"),
            type_id: r.get("type_id"),
            location_id: r.get("location_id"),
            x: r.get("x"),
            y: r.get("y"),
            z: r.get("z"),
            memo: r.get("memo"),
            note: r.get("note"),
            created: r.get("created"),
            expiry: r.get("expiry"),
            creator_id: r.get("creator_id"),
        }
    }
}
struct Exists { exists: bool }
impl Exists {
    fn from_row(r: &Row) -> Self { Self { exists: r.get("exists") } }
}
struct BookmarkId { bookmark_id: i64 }
impl BookmarkId {
    fn from_row(r: &Row) -> Self { Self { bookmark_id: r.get("bookmark_id") } }
}
pub(crate) struct Moved {
    pub bookmark_id: i64,
    pub folder_id: i64,
    pub subfolder_id: Option<i64>,
}
impl Moved {
    fn from_row(r: &Row) -> Self {
        Self { bookmark_id: r.get("bookmark_id"), folder_id: r.get("folder_id"), subfolder_id: r.get("subfolder_id") }
    }
}
const BOOKMARK_SELECT: &str = "SELECT bookmark_id, folder_id, subfolder_id, item_id, type_id, location_id, x, y, z, memo, note, created, expiry, creator_id
    FROM game.bookmarks";

pub(crate) async fn owns_folder(ex: &(impl Executor + ?Sized), me: i64, folder: i64) -> Result<bool, Error> {
    let row = ex.query_one(
        "SELECT EXISTS (SELECT 1 FROM game.bookmark_folders WHERE folder_id = $1 AND owner_id = $2)",
        &[&folder, &me],
    ).await?;
    Ok(Exists::from_row(&row).exists)
}
pub(crate) struct NewBookmark<'a> {
    pub folder: i64,
    pub subfolder: Option<i64>,
    pub creator: i64,
    pub item: Option<i64>,
    pub type_id: Option<i32>,
    pub system: i64,
    pub pos: [f64; 3],
    pub memo: &'a str,
    pub note: &'a str,
    pub created: i64,
    pub expiry: Option<i64>,
}
pub(crate) async fn add(ex: &impl Executor, b: &NewBookmark<'_>) -> Result<i64, Error> {
    let row = ex.query_one(
        "INSERT INTO game.bookmarks (folder_id, subfolder_id, creator_id, item_id, type_id, location_id, x, y, z, memo, note, created, expiry)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) RETURNING bookmark_id",
        &[&b.folder, &b.subfolder, &b.creator, &b.item, &b.type_id, &b.system, &b.pos[0], &b.pos[1], &b.pos[2], &b.memo, &b.note, &b.created, &b.expiry],
    ).await?;
    Ok(BookmarkId::from_row(&row).bookmark_id)
}
pub(crate) async fn folders(ex: &impl Executor, me: i64) -> Result<Vec<Folder>, Error> {
    let rows = ex.query(
        "SELECT folder_id, name, description FROM game.bookmark_folders WHERE owner_id = $1 ORDER BY folder_id",
        &[&me],
    ).await?;
    Ok(rows.iter().map(Folder::from_row).collect())
}
pub(crate) async fn bookmarks(ex: &impl Executor, me: i64, now: i64) -> Result<Vec<Bookmark>, Error> {
    let rows = ex.query(
        &format!("{BOOKMARK_SELECT} WHERE folder_id IN (SELECT folder_id FROM game.bookmark_folders WHERE owner_id = $1) AND (expiry IS NULL OR expiry > $2)"),
        &[&me, &now],
    ).await?;
    Ok(rows.iter().map(Bookmark::from_row).collect())
}
pub(crate) async fn subfolders(ex: &impl Executor, me: i64) -> Result<Vec<Subfolder>, Error> {
    let rows = ex.query(
        "SELECT subfolder_id, folder_id, name FROM game.bookmark_subfolders WHERE folder_id IN (SELECT folder_id FROM game.bookmark_folders WHERE owner_id = $1)",
        &[&me],
    ).await?;
    Ok(rows.iter().map(Subfolder::from_row).collect())
}
pub(crate) async fn add_folder(ex: &impl Executor, me: i64, name: &str, description: &str) -> Result<Folder, Error> {
    let row = ex.query_one(
        "INSERT INTO game.bookmark_folders (owner_id, name, description) VALUES ($1, $2, $3) RETURNING folder_id, name, description",
        &[&me, &name, &description],
    ).await?;
    Ok(Folder::from_row(&row))
}
pub(crate) async fn update_folder(ex: &impl Executor, folder: i64, me: i64, name: &str, description: &str) -> Result<u64, Error> {
    ex.execute(
        "UPDATE game.bookmark_folders SET name = $3, description = $4 WHERE folder_id = $1 AND owner_id = $2",
        &[&folder, &me, &name, &description],
    ).await
}
pub(crate) async fn delete_folder(ex: &impl Executor, folder: i64) -> Result<(), Error> {
    for sql in [
        "DELETE FROM game.bookmarks WHERE folder_id = $1",
        "DELETE FROM game.bookmark_subfolders WHERE folder_id = $1",
        "DELETE FROM game.bookmark_folders WHERE folder_id = $1",
    ] {
        ex.execute(sql, &[&folder]).await?;
    }
    Ok(())
}
pub(crate) async fn folder(ex: &impl Executor, folder: i64, me: i64) -> Result<Option<Folder>, Error> {
    let row = ex.query_opt(
        "SELECT folder_id, name, description FROM game.bookmark_folders WHERE folder_id = $1 AND owner_id = $2",
        &[&folder, &me],
    ).await?;
    Ok(row.as_ref().map(Folder::from_row))
}
pub(crate) async fn add_subfolder(ex: &impl Executor, folder: i64, name: &str) -> Result<Subfolder, Error> {
    let row = ex.query_one(
        "INSERT INTO game.bookmark_subfolders (folder_id, name) VALUES ($1, $2) RETURNING subfolder_id, folder_id, name",
        &[&folder, &name],
    ).await?;
    Ok(Subfolder::from_row(&row))
}
pub(crate) async fn update_subfolder(ex: &impl Executor, folder: i64, subfolder: i64, name: &str) -> Result<u64, Error> {
    ex.execute(
        "UPDATE game.bookmark_subfolders SET name = $3 WHERE subfolder_id = $2 AND folder_id = $1",
        &[&folder, &subfolder, &name],
    ).await
}
pub(crate) async fn delete_subfolder_bookmarks(ex: &impl Executor, folder: i64, subfolder: i64) -> Result<Vec<i64>, Error> {
    let rows = ex.query(
        "DELETE FROM game.bookmarks WHERE folder_id = $1 AND subfolder_id = $2 RETURNING bookmark_id",
        &[&folder, &subfolder],
    ).await?;
    Ok(rows.iter().map(|r| BookmarkId::from_row(r).bookmark_id).collect())
}
pub(crate) async fn delete_subfolder(ex: &impl Executor, folder: i64, subfolder: i64) -> Result<u64, Error> {
    ex.execute("DELETE FROM game.bookmark_subfolders WHERE subfolder_id = $1 AND folder_id = $2", &[&subfolder, &folder]).await
}
pub(crate) struct UpdateBookmark<'a> {
    pub id: i64,
    pub folder: i64,
    pub memo: &'a str,
    pub note: &'a str,
    pub subfolder: Option<i64>,
    pub new_folder: i64,
    pub cancel: bool,
    pub me: i64,
}
pub(crate) async fn update_bookmark(ex: &impl Executor, b: &UpdateBookmark<'_>) -> Result<u64, Error> {
    ex.execute(
        "UPDATE game.bookmarks SET memo = $3, note = $4, subfolder_id = $5, folder_id = $6, expiry = CASE WHEN $7 THEN NULL ELSE expiry END
                     WHERE bookmark_id = $1 AND folder_id = $2 AND folder_id IN (SELECT folder_id FROM game.bookmark_folders WHERE owner_id = $8)",
        &[&b.id, &b.folder, &b.memo, &b.note, &b.subfolder, &b.new_folder, &b.cancel, &b.me],
    ).await
}
pub(crate) async fn delete_bookmarks(ex: &impl Executor, folder: i64, wanted: &[i64], me: i64) -> Result<Vec<i64>, Error> {
    let rows = ex.query(
        "DELETE FROM game.bookmarks WHERE folder_id = $1 AND bookmark_id = ANY($2)
                     AND folder_id IN (SELECT folder_id FROM game.bookmark_folders WHERE owner_id = $3) RETURNING bookmark_id",
        &[&folder, &wanted, &me],
    ).await?;
    Ok(rows.iter().map(|r| BookmarkId::from_row(r).bookmark_id).collect())
}
pub(crate) async fn move_bookmarks(ex: &impl Executor, from: i64, to: i64, sub: Option<i64>, wanted: &[i64], me: i64) -> Result<Vec<Moved>, Error> {
    let rows = ex.query(
        "UPDATE game.bookmarks SET folder_id = $2, subfolder_id = $3 WHERE folder_id = $1 AND bookmark_id = ANY($4)
                     AND folder_id IN (SELECT folder_id FROM game.bookmark_folders WHERE owner_id = $5) RETURNING bookmark_id, folder_id, subfolder_id",
        &[&from, &to, &sub, &wanted, &me],
    ).await?;
    Ok(rows.iter().map(Moved::from_row).collect())
}
pub(crate) async fn copy_bookmarks(ex: &impl Executor, from: i64, to: i64, sub: Option<i64>, me: i64, wanted: &[i64]) -> Result<Vec<Bookmark>, Error> {
    let rows = ex.query(
        "INSERT INTO game.bookmarks (folder_id, subfolder_id, creator_id, item_id, type_id, location_id, x, y, z, memo, note, created, expiry)
                     SELECT $2, $3, $4, item_id, type_id, location_id, x, y, z, memo, note, created, expiry FROM game.bookmarks
                     WHERE folder_id = $1 AND bookmark_id = ANY($5) AND folder_id IN (SELECT folder_id FROM game.bookmark_folders WHERE owner_id = $4)
                     RETURNING bookmark_id, folder_id, subfolder_id, item_id, type_id, location_id, x, y, z, memo, note, created, expiry, creator_id",
        &[&from, &to, &sub, &me, &wanted],
    ).await?;
    Ok(rows.iter().map(Bookmark::from_row).collect())
}
