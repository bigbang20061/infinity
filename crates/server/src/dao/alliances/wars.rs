//! Alliance name search for mutual-war opponent selection in `war_cso.py`.
//! Reads live alliances from `game.alliances`; deleted alliances are excluded.
//! Matching is case-insensitive exact or literal substring, without LIKE wildcards.
use crate::db::{executor::Executor, Error};
use tokio_postgres::Row;

pub(crate) struct Candidate { pub id: i64, pub name: String }
impl Candidate {
    fn from_row(row: &Row) -> Self { Self { id: row.get("alliance_id"), name: row.get("name") } }
}
pub(crate) async fn search(ex: &(impl Executor + ?Sized), text: &str, exact: bool) -> Result<Vec<Candidate>, Error> {
    Ok(ex.query("SELECT alliance_id, name FROM game.alliances WHERE NOT deleted AND CASE WHEN $2 THEN lower(name) = lower($1) ELSE position(lower($1) in lower(name)) > 0 END ORDER BY name, alliance_id", &[&text, &exact]).await?.iter().map(Candidate::from_row).collect())
}
