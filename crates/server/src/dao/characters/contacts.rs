//! Persistent personal contacts and blocks for the client address book
//! (`eve/client/script/ui/shared/neocom/addressBook/addressbookService.py`). Uses `game.char_contacts`
//! and `game.char_blocked`; reverse watcher lookups include only watchlisted contacts.
//! Label management, transactions and notifications belong to callers and are not
//! implemented here.

use crate::db::{executor::Executor, Error};
use tokio_postgres::Row;

pub(crate) struct Contact {
    pub contact_id: i64,
    pub relationship: f32,
    pub in_watchlist: bool,
    pub label_mask: i64,
}

impl Contact {
    fn from_row(row: &Row) -> Self {
        Self {
            contact_id: row.get("contact_id"),
            relationship: row.get("relationship"),
            in_watchlist: row.get("in_watchlist"),
            label_mask: row.get("label_mask"),
        }
    }
}

struct OwnerId { owner_id: i64 }
impl OwnerId {
    fn from_row(row: &Row) -> Self { Self { owner_id: row.get("owner_id") } }
}

pub(crate) async fn list(ex: &(impl Executor + ?Sized), char_id: i64) -> Result<Vec<Contact>, Error> {
    let rows = ex.query(
        "SELECT contact_id, relationship, in_watchlist, label_mask FROM game.char_contacts WHERE char_id = $1 ORDER BY contact_id",
        &[&char_id],
    ).await?;
    Ok(rows.iter().map(Contact::from_row).collect())
}

pub(crate) async fn blocked(ex: &(impl Executor + ?Sized), char_id: i64) -> Result<Vec<i64>, Error> {
    let rows = ex.query(
        "SELECT owner_id FROM game.char_blocked WHERE char_id = $1 ORDER BY owner_id",
        &[&char_id],
    ).await?;
    Ok(rows.iter().map(|row| OwnerId::from_row(row).owner_id).collect())
}

pub(crate) async fn watchers_of(ex: &(impl Executor + ?Sized), contact_id: i64) -> Result<Vec<i64>, Error> {
    let rows = ex.query(
        "SELECT char_id AS owner_id FROM game.char_contacts WHERE contact_id = $1 AND in_watchlist ORDER BY char_id",
        &[&contact_id],
    ).await?;
    Ok(rows.iter().map(|row| OwnerId::from_row(row).owner_id).collect())
}

pub(crate) async fn upsert(ex: &(impl Executor + ?Sized), char_id: i64, contact_id: i64, relationship: f32, in_watchlist: bool) -> Result<(), Error> {
    ex.execute(
        "INSERT INTO game.char_contacts (char_id, contact_id, relationship, in_watchlist) VALUES ($1, $2, $3, $4)
         ON CONFLICT (char_id, contact_id) DO UPDATE SET relationship = EXCLUDED.relationship, in_watchlist = EXCLUDED.in_watchlist",
        &[&char_id, &contact_id, &relationship, &in_watchlist],
    ).await?;
    Ok(())
}

pub(crate) async fn edit_relationships(ex: &(impl Executor + ?Sized), char_id: i64, ids: &[i64], relationship: f32) -> Result<Vec<Contact>, Error> {
    let mut ids = ids.to_vec();
    ids.sort_unstable();
    ids.dedup();
    let mut contacts = Vec::new();
    for id in ids {
        if let Some(row) = ex.query_opt(
            "UPDATE game.char_contacts SET relationship = $3 WHERE char_id = $1 AND contact_id = $2 RETURNING contact_id, relationship, in_watchlist, label_mask",
            &[&char_id, &id, &relationship],
        ).await? {
            contacts.push(Contact::from_row(&row));
        }
    }
    Ok(contacts)
}

pub(crate) async fn delete(ex: &(impl Executor + ?Sized), char_id: i64, ids: &[i64]) -> Result<Vec<Contact>, Error> {
    let mut ids = ids.to_vec();
    ids.sort_unstable();
    ids.dedup();
    let mut contacts = Vec::new();
    for id in ids {
        if let Some(row) = ex.query_opt(
            "DELETE FROM game.char_contacts WHERE char_id = $1 AND contact_id = $2 RETURNING contact_id, relationship, in_watchlist, label_mask",
            &[&char_id, &id],
        ).await? {
            contacts.push(Contact::from_row(&row));
        }
    }
    Ok(contacts)
}

pub(crate) async fn set_blocked(ex: &(impl Executor + ?Sized), char_id: i64, ids: &[i64], blocked: bool) -> Result<Vec<Contact>, Error> {
    let mut ids = ids.to_vec();
    ids.sort_unstable();
    ids.dedup();
    let mut contacts = Vec::new();
    for id in ids {
        let changed = if blocked {
            ex.query_opt(
                "INSERT INTO game.char_blocked (char_id, owner_id) VALUES ($1, $2) ON CONFLICT DO NOTHING RETURNING owner_id",
                &[&char_id, &id],
            ).await?.is_some()
        } else {
            ex.query_opt(
                "DELETE FROM game.char_blocked WHERE char_id = $1 AND owner_id = $2 RETURNING owner_id",
                &[&char_id, &id],
            ).await?.is_some()
        };
        if changed {
            if let Some(row) = ex.query_opt(
                "SELECT contact_id, relationship, in_watchlist, label_mask FROM game.char_contacts WHERE char_id = $1 AND contact_id = $2",
                &[&char_id, &id],
            ).await? {
                contacts.push(Contact::from_row(&row));
            }
        }
    }
    Ok(contacts)
}
