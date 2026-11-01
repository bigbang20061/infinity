//! Per-character known folder persistence for `bookmarkSvc.py`.
//! Uses `game.bookmark_known_folders`; access policy belongs to the service.
use crate::db::{executor::Executor, Error};
use tokio_postgres::Row;

#[derive(Clone)]
pub(crate) struct KnownFolder { pub character_id: i64, pub folder_id: i64, pub is_active: bool }
impl KnownFolder {
    fn from_row(r: &Row) -> Self {
        Self { character_id: r.get("character_id"), folder_id: r.get("folder_id"), is_active: r.get("is_active") }
    }
}
pub(crate) async fn known(ex: &(impl Executor + ?Sized), character: i64) -> Result<Vec<KnownFolder>, Error> {
    Ok(ex.query("SELECT character_id, folder_id, is_active FROM game.bookmark_known_folders WHERE character_id = $1 ORDER BY folder_id", &[&character]).await?.iter().map(KnownFolder::from_row).collect())
}
pub(crate) async fn known_for_folder(ex: &(impl Executor + ?Sized), folder: i64) -> Result<Vec<KnownFolder>, Error> {
    Ok(ex.query("SELECT character_id, folder_id, is_active FROM game.bookmark_known_folders WHERE folder_id = $1 ORDER BY character_id", &[&folder]).await?.iter().map(KnownFolder::from_row).collect())
}
pub(crate) async fn set_known(ex: &(impl Executor + ?Sized), character: i64, folder: i64, active: bool) -> Result<u64, Error> {
    ex.execute("INSERT INTO game.bookmark_known_folders (character_id, folder_id, is_active) VALUES ($1, $2, $3) ON CONFLICT (character_id, folder_id) DO UPDATE SET is_active = EXCLUDED.is_active", &[&character, &folder, &active]).await
}
pub(crate) async fn remove_known(ex: &(impl Executor + ?Sized), character: i64, folder: i64) -> Result<u64, Error> {
    ex.execute("DELETE FROM game.bookmark_known_folders WHERE character_id = $1 AND folder_id = $2", &[&character, &folder]).await
}
