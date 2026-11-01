//! Durable industry readiness for `eve/client/script/industry/industrySvc.py`.
//! Uses game.industry_jobs and game.jobs; deadlines only mark jobs ready, never deliver output.
//! Structures and reactions are not supported.
use super::{db_err, filetime_now, industry_dao, lock, tell_job, CallError, Job, Outbox, INDUSTRY_JOB_STATUS_INSTALLED, INDUSTRY_JOB_STATUS_READY};
use crate::{db::{executor::Executor, outbox::dispatch_world, Db}, scheduler, space::World};
use std::{future::Future, pin::Pin, sync::Arc};

pub(crate) const KIND: &str = "industry.ready";

fn deadline(id: i64, due_at: i64) -> scheduler::Job {
    scheduler::Job { id: format!("{KIND}:{id}"), kind: KIND.into(), due_at, payload: id.to_string() }
}

pub(super) async fn schedule(ex: &(impl Executor + ?Sized), job: &Job) -> Result<(), CallError> {
    scheduler::schedule(ex, &deadline(job.id, job.end)).await.map_err(db_err)?;
    Ok(())
}

pub(super) async fn cancel(ex: &(impl Executor + ?Sized), id: i64) -> Result<(), CallError> {
    scheduler::cancel(ex, &deadline(id, 0).id).await.map_err(db_err)?;
    Ok(())
}

pub(crate) async fn seed(db: &Db) -> Result<(), CallError> {
    let _market = lock().await;
    let now = filetime_now();
    db.transaction(|tx, _outbox: &mut Outbox| Box::pin(async move {
        for row in industry_dao::installed_deadlines(tx).await.map_err(db_err)? {
            crate::dao::jobs::seed(tx, &deadline(row.id, row.end), now).await.map_err(db_err)?;
        }
        Ok(())
    }), |value, _outbox| async move { scheduler::wake(); Ok(value) }).await
}

pub(crate) fn handle(db: Db, world: Arc<World>, job: scheduler::Job) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> {
    Box::pin(async move {
        let id: i64 = serde_json::from_str(&job.payload)?;
        let _market = lock().await;
        let now = filetime_now();
        let result = db.transaction(|tx, outbox: &mut Outbox| Box::pin(async move {
            let Some(mut job) = industry_dao::job(tx, id).await.map_err(db_err)? else { return Ok(()) };
            if job.status != INDUSTRY_JOB_STATUS_INSTALLED { return Ok(()); }
            if job.end > now { return Err(CallError::Internal(anyhow::anyhow!("industry deadline invoked before due: {id}"))); }
            if industry_dao::mark_ready(tx, id, now).await.map_err(db_err)? != 0 {
                job.status = INDUSTRY_JOB_STATUS_READY;
                tell_job(tx, outbox, &job).await?;
            }
            Ok(())
        }), |value, outbox| {
            let world = &world;
            async move { dispatch_world(value, outbox, world) }
        }).await;
        result.map_err(|error| match error {
            CallError::Internal(error) => error,
            CallError::User { msg, .. } => anyhow::anyhow!("industry readiness refused: {msg}"),
        })
    })
}
