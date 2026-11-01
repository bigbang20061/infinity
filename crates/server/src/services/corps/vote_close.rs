//! Durable close of informational corporation votes, mirroring corporation voting deadlines.
//! Uses game.corp_vote_cases and the scheduler's game.jobs; no winner or action is executed.

use super::*;
use crate::dao::corporations::votes as dao;
use crate::scheduler::Job;
use std::{future::Future, pin::Pin};

pub(crate) const KIND: &str = "corp.vote.close";

pub(super) async fn close_inner<X: Send>(ex: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, id: i64, now: i64) -> Result<(), CallError> {
    let Some(case) = dao::lock_case(ex, id).await.map_err(db_err)? else { return Ok(()); };
    if case.closed_at.is_some() { return Ok(()); }
    if case.ends > now { return Err(CallError::Internal(anyhow::anyhow!("vote close before deadline"))); }
    if dao::close(ex, id, now).await.map_err(db_err)? {
        super::votes::notify(ex, outbox, case.corporation_id, id, "OnVoteCaseChanged").await?;
    }
    Ok(())
}

pub(crate) fn handle(db: Db, world: Arc<World>, job: Job) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> {
    Box::pin(async move {
        let id: i64 = serde_json::from_str(&job.payload)?;
        db.transaction(|tx, outbox: &mut Outbox| Box::pin(async move {
            close_inner(tx, outbox, id, filetime_now()).await
        }), |value, outbox| async move { dispatch_world(value, outbox, &world) }).await.map_err(|error| match error {
            CallError::Internal(error) => error,
            CallError::User { msg, .. } => anyhow::anyhow!("vote close refused: {msg}"),
        })
    })
}
