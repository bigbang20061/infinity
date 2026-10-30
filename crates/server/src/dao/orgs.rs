//! Persistence for corporation and alliance bulletins, contacts and labels.
//! Mirrors corpRegistry and allianceRegistry client organization calls using
//! game.org_bulletins, game.org_contacts and game.org_labels. Authorization,
//! notifications and other organization features are outside this module.
use crate::db::{executor::Executor, Error};
use tokio_postgres::Row;

pub(crate) struct Bulletin { pub id: i64, pub title: String, pub body: String, pub edit_char_id: i64, pub edit_date: i64, pub sort_order: i32 }
impl Bulletin { fn from_row(r: &Row) -> Self { Self { id: r.get("bulletin_id"), title: r.get("title"), body: r.get("body"), edit_char_id: r.get("edit_char_id"), edit_date: r.get("edit_date"), sort_order: r.get("sort_order") } } }
pub(crate) struct Contact { pub id: i64, pub relationship: f64, pub label_mask: i64 }
impl Contact { fn from_row(r: &Row) -> Self { Self { id: r.get("contact_id"), relationship: r.get("relationship_id"), label_mask: r.get("label_mask") } } }
pub(crate) struct Label { pub id: i64, pub name: String, pub color: Option<i32> }
impl Label { fn from_row(r: &Row) -> Self { Self { id: r.get("label_id"), name: r.get("name"), color: r.get("color") } } }

struct EditDate { value: i64 }
impl EditDate {
    fn from_row(row: &Row) -> Self { Self { value: row.get("edit_date") } }
}
struct BulletinCount { value: i64 }
impl BulletinCount {
    fn from_row(row: &Row) -> Self { Self { value: row.get("count") } }
}
struct LabelId { value: i64 }
impl LabelId {
    fn from_row(row: &Row) -> Self { Self { value: row.get("label_id") } }
}

pub(crate) async fn bulletins(ex: &(impl Executor + ?Sized), owner: i64) -> Result<Vec<Bulletin>, Error> {
    Ok(ex.query("SELECT bulletin_id, title, body, edit_char_id, edit_date, sort_order FROM game.org_bulletins WHERE owner_id = $1 ORDER BY sort_order, edit_date DESC", &[&owner]).await?.iter().map(Bulletin::from_row).collect())
}
pub(crate) async fn edit_date(ex: &(impl Executor + ?Sized), id: i64, owner: i64) -> Result<Option<i64>, Error> {
    Ok(ex.query_opt("SELECT edit_date FROM game.org_bulletins WHERE bulletin_id = $1 AND owner_id = $2", &[&id, &owner]).await?.map(|r| EditDate::from_row(&r).value))
}
pub(crate) async fn update_bulletin(ex: &(impl Executor + ?Sized), id: i64, title: &str, body: &str, me: i64, now: i64) -> Result<u64, Error> {
    ex.execute("UPDATE game.org_bulletins SET title = $2, body = $3, edit_char_id = $4, edit_date = $5 WHERE bulletin_id = $1", &[&id, &title, &body, &me, &now]).await
}
pub(crate) async fn bulletin_count(ex: &(impl Executor + ?Sized), owner: i64) -> Result<i64, Error> {
    Ok(BulletinCount::from_row(&ex.query_one("SELECT count(*) AS count FROM game.org_bulletins WHERE owner_id = $1", &[&owner]).await?).value)
}
pub(crate) async fn insert_bulletin(ex: &(impl Executor + ?Sized), owner: i64, title: &str, body: &str, me: i64, now: i64) -> Result<u64, Error> {
    ex.execute("INSERT INTO game.org_bulletins (owner_id, title, body, edit_char_id, edit_date, sort_order)
                     VALUES ($1, $2, $3, $4, $5, (SELECT COALESCE(max(sort_order) + 1, 0) FROM game.org_bulletins WHERE owner_id = $1))", &[&owner, &title, &body, &me, &now]).await
}
pub(crate) async fn delete_bulletin(ex: &(impl Executor + ?Sized), id: i64, owner: i64) -> Result<u64, Error> { ex.execute("DELETE FROM game.org_bulletins WHERE bulletin_id = $1 AND owner_id = $2", &[&id, &owner]).await }
pub(crate) async fn order_bulletins(ex: &(impl Executor + ?Sized), owner: i64, ids: &[i64]) -> Result<(), Error> {
    for (i, id) in ids.iter().enumerate() {
        ex.execute("UPDATE game.org_bulletins SET sort_order = $3 WHERE bulletin_id = $1 AND owner_id = $2", &[id, &owner, &(i as i32)]).await?;
    }
    Ok(())
}
pub(crate) async fn contacts(ex: &(impl Executor + ?Sized), owner: i64) -> Result<Vec<Contact>, Error> { Ok(ex.query("SELECT contact_id, relationship_id, label_mask FROM game.org_contacts WHERE owner_id = $1", &[&owner]).await?.iter().map(Contact::from_row).collect()) }
pub(crate) async fn upsert_contact(ex: &(impl Executor + ?Sized), owner: i64, id: i64, rel: f64) -> Result<u64, Error> { ex.execute("INSERT INTO game.org_contacts (owner_id, contact_id, relationship_id) VALUES ($1, $2, $3)
                     ON CONFLICT (owner_id, contact_id) DO UPDATE SET relationship_id = $3", &[&owner, &id, &rel]).await }
pub(crate) async fn delete_contacts(ex: &(impl Executor + ?Sized), owner: i64, ids: &[i64]) -> Result<u64, Error> { ex.execute("DELETE FROM game.org_contacts WHERE owner_id = $1 AND contact_id = ANY($2)", &[&owner, &ids]).await }
pub(crate) async fn update_relationship(ex: &(impl Executor + ?Sized), owner: i64, ids: &[i64], rel: f64) -> Result<u64, Error> { ex.execute("UPDATE game.org_contacts SET relationship_id = $3 WHERE owner_id = $1 AND contact_id = ANY($2)", &[&owner, &ids, &rel]).await }
pub(crate) async fn labels(ex: &(impl Executor + ?Sized), owner: i64) -> Result<Vec<Label>, Error> { Ok(ex.query("SELECT label_id, name, color FROM game.org_labels WHERE owner_id = $1", &[&owner]).await?.iter().map(Label::from_row).collect()) }
pub(crate) async fn label_ids(ex: &(impl Executor + ?Sized), owner: i64) -> Result<Vec<i64>, Error> { Ok(ex.query("SELECT label_id FROM game.org_labels WHERE owner_id = $1", &[&owner]).await?.iter().map(|r| LabelId::from_row(r).value).collect()) }
pub(crate) async fn insert_label(ex: &(impl Executor + ?Sized), owner: i64, id: i64, name: &str, color: Option<i32>) -> Result<u64, Error> { ex.execute("INSERT INTO game.org_labels (owner_id, label_id, name, color) VALUES ($1, $2, $3, $4)", &[&owner, &id, &name, &color]).await }
pub(crate) async fn clear_label(ex: &(impl Executor + ?Sized), owner: i64, id: i64) -> Result<u64, Error> { ex.execute("UPDATE game.org_contacts SET label_mask = label_mask & ~$2 WHERE owner_id = $1", &[&owner, &id]).await }
pub(crate) async fn delete_label(ex: &(impl Executor + ?Sized), id: i64, owner: i64) -> Result<u64, Error> { ex.execute("DELETE FROM game.org_labels WHERE label_id = $1 AND owner_id = $2", &[&id, &owner]).await }
pub(crate) async fn edit_label(ex: &(impl Executor + ?Sized), id: i64, owner: i64, name: Option<String>, color: Option<i32>) -> Result<u64, Error> { ex.execute("UPDATE game.org_labels SET name = COALESCE($3, name), color = COALESCE($4, color) WHERE label_id = $1 AND owner_id = $2", &[&id, &owner, &name, &color]).await }
pub(crate) enum LabelChange { Assign, Remove }
pub(crate) async fn change_labels(ex: &(impl Executor + ?Sized), owner: i64, ids: &[i64], mask: i64, change: LabelChange) -> Result<u64, Error> {
    let sql = match change {
        LabelChange::Assign => "UPDATE game.org_contacts SET label_mask = label_mask | $3 WHERE owner_id = $1 AND contact_id = ANY($2)",
        LabelChange::Remove => "UPDATE game.org_contacts SET label_mask = label_mask & ~$3 WHERE owner_id = $1 AND contact_id = ANY($2)",
    };
    ex.execute(sql, &[&owner, &ids, &mask]).await
}
