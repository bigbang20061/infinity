//! Test-only inspection of durable job state in `game.jobs`; no client counterpart.
//! These helpers do not modify jobs or implement scheduler behavior.
use crate::db::{Db, Error};
use tokio_postgres::Row;

pub(crate) struct JobState {
    pub(crate) state: String,
    pub(crate) attempt: i32,
    pub(crate) last_error: Option<String>,
    pub(crate) due_at: i64,
}

impl JobState {
    fn from_row(row: &Row) -> Self {
        Self {
            state: row.get("state"),
            attempt: row.get("attempt"),
            last_error: row.get("last_error"),
            due_at: row.get("due_at"),
        }
    }
}

pub(crate) async fn inspect(db: &Db, id: &str) -> Result<JobState, Error> {
    let row = db.query_one("SELECT state,attempt,last_error,due_at FROM game.jobs WHERE job_id=$1", &[&id]).await?;
    Ok(JobState::from_row(&row))
}
